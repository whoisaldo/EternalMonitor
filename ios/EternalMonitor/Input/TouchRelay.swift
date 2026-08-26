import CoreGraphics
import Foundation

/// Maps view-space touch points onto the displayed video's content rect and
/// normalizes them to the wire's 0...65535 space. Pure and unit-tested — the
/// letterbox math must mirror `MetalRenderer.aspectFitScale`.
struct ContentRectMapper {
    var viewSize: CGSize
    var videoSize: CGSize

    /// The aspect-fit rectangle (in view coordinates) the video occupies.
    var contentRect: CGRect {
        guard viewSize.width > 0, viewSize.height > 0,
              videoSize.width > 0, videoSize.height > 0 else { return .zero }
        let videoAspect = videoSize.width / videoSize.height
        let viewAspect = viewSize.width / viewSize.height
        if videoAspect > viewAspect {
            // Video wider than view: full width, letterboxed top/bottom.
            let height = viewSize.width / videoAspect
            return CGRect(x: 0, y: (viewSize.height - height) / 2, width: viewSize.width, height: height)
        } else {
            // Video taller: full height, pillarboxed left/right.
            let width = viewSize.height * videoAspect
            return CGRect(x: (viewSize.width - width) / 2, y: 0, width: width, height: viewSize.height)
        }
    }

    /// Normalize a view point. Returns nil while no video is up, or when the
    /// touch lands in the letterbox bars (those must not click anything).
    func normalize(_ point: CGPoint) -> (x: UInt16, y: UInt16)? {
        let rect = contentRect
        guard rect.width > 0, rect.height > 0 else { return nil }
        guard rect.insetBy(dx: -1, dy: -1).contains(point) else { return nil }
        let nx = (point.x - rect.minX) / rect.width
        let ny = (point.y - rect.minY) / rect.height
        return (
            UInt16((nx.clamped01 * 65535).rounded()),
            UInt16((ny.clamped01 * 65535).rounded())
        )
    }
}

private extension CGFloat {
    var clamped01: CGFloat { Swift.min(1, Swift.max(0, self)) }
}

/// Turns raw multitouch into wire input events: tap → click, drag → press +
/// move + release, two fingers → scroll, long-press → right click, pencil →
/// immediate press (ink can't wait out a tap/drag disambiguation window).
/// Pure — the hosting view feeds it normalized touches and a hold timer, and
/// executes the outputs it returns.
///
/// Edge events (press/release) are emitted twice with one `eventId` — the
/// wire's loss-tolerance idiom; the host injects each id once.
struct TouchRelayMachine {
    enum Output: Equatable {
        case send(WireInputEvent)
        case toggleHUD
    }

    enum Kind {
        static let touch: UInt8 = 0
        static let pencil: UInt8 = 1
        static let scroll: UInt8 = 3
    }

    enum Phase {
        static let began: UInt8 = 0
        static let moved: UInt8 = 1
        static let ended: UInt8 = 2
        static let cancelled: UInt8 = 3
    }

    private enum Mode: Equatable {
        case idle
        /// One finger down, not yet committed to tap vs drag vs right-click.
        case pending(start: Point, isPencil: Bool)
        case leftDown(isPencil: Bool)
        case rightDown
        case scrolling(lastCentroid: Point)
        /// Three-plus fingers or a mid-gesture wipe: ignore until all lift.
        case suppressed
    }

    struct Point: Equatable {
        var x: UInt16
        var y: UInt16
    }

    /// Movement beyond this (in 0...65535 norm units, ~0.5% of the screen)
    /// commits a pending touch to a drag instead of a tap.
    static let dragSlop: Int32 = 350
    /// Minimum spacing between relayed move/scroll events (~240 Hz).
    static let moveIntervalUs: UInt64 = 4_166
    /// A stationary press this long becomes a right click.
    static let holdRightClickUs: UInt64 = 500_000

    /// Video pixels per full normalized axis — converts two-finger pan deltas
    /// into the wire's pixel-ish scroll units. Updated by the host view.
    var videoPixelSize: CGSize = .zero

    private var mode: Mode = .idle
    private var touchCount = 0
    private var nextEventId: UInt32 = 0
    private var lastMoveSentUs: UInt64 = 0
    private var pendingSinceUs: UInt64 = 0
    private var scrollRemainder: CGSize = .zero

    private mutating func edge(
        _ phase: UInt8, kind: UInt8, buttons: UInt8, at p: Point, timeUs: UInt64,
        pressure: UInt16 = 0
    ) -> [Output] {
        nextEventId &+= 1
        let event = WireInputEvent(
            kind: kind, phase: phase, buttons: buttons, eventId: nextEventId,
            xNorm: p.x, yNorm: p.y, pressureX1000: pressure, clientTimeUs: timeUs
        )
        return [.send(event), .send(event)]
    }

    private mutating func move(
        kind: UInt8, buttons: UInt8, at p: Point, timeUs: UInt64,
        pressure: UInt16 = 0, dx: Int16 = 0, dy: Int16 = 0
    ) -> [Output] {
        nextEventId &+= 1
        return [.send(WireInputEvent(
            kind: kind, phase: Phase.moved, buttons: buttons, eventId: nextEventId,
            xNorm: p.x, yNorm: p.y, pressureX1000: pressure,
            scrollDx: dx, scrollDy: dy, clientTimeUs: timeUs
        ))]
    }

    // MARK: - Inputs

    mutating func touchBegan(
        at point: Point?, isPencil: Bool, timeUs: UInt64
    ) -> [Output] {
        touchCount += 1
        switch (mode, touchCount) {
        case (.idle, 1):
            guard let point else {
                mode = .suppressed
                return []
            }
            if isPencil {
                mode = .leftDown(isPencil: true)
                return edge(Phase.began, kind: Kind.pencil, buttons: 1, at: point, timeUs: timeUs)
            }
            mode = .pending(start: point, isPencil: false)
            pendingSinceUs = timeUs
            return []
        case (.pending, 2):
            // Second finger before commit: this is a scroll, not a click.
            mode = .scrolling(lastCentroid: point ?? Point(x: 32767, y: 32767))
            scrollRemainder = .zero
            return []
        case (.leftDown(let isPencil), 2) where !isPencil:
            // Finger drag joined by a second finger: release, then scroll.
            let release = edge(Phase.ended, kind: Kind.touch, buttons: 1,
                               at: point ?? Point(x: 32767, y: 32767), timeUs: timeUs)
            mode = .scrolling(lastCentroid: point ?? Point(x: 32767, y: 32767))
            scrollRemainder = .zero
            return release
        case (.scrolling, _):
            return []
        default:
            // Third finger (or anything unexpected): abort the gesture.
            return suppress(timeUs: timeUs)
        }
    }

    /// `centroid` is the mean of all active touches (the machine tracks
    /// scrolls by centroid so two uneven fingers still scroll smoothly).
    mutating func touchMoved(
        to point: Point?, centroid: Point?, isPencil: Bool, force: CGFloat, timeUs: UInt64
    ) -> [Output] {
        switch mode {
        case .pending(let start, _):
            guard let point else { return [] }
            let dx = Int32(point.x) - Int32(start.x)
            let dy = Int32(point.y) - Int32(start.y)
            if dx * dx + dy * dy > Self.dragSlop * Self.dragSlop {
                // Committed to a drag: press at the start point, catch up.
                mode = .leftDown(isPencil: false)
                var outputs = edge(Phase.began, kind: Kind.touch, buttons: 1, at: start, timeUs: timeUs)
                lastMoveSentUs = timeUs
                outputs += move(kind: Kind.touch, buttons: 1, at: point, timeUs: timeUs)
                return outputs
            }
            return []
        case .leftDown(let isPencil):
            guard let point, timeUs &- lastMoveSentUs >= Self.moveIntervalUs else { return [] }
            lastMoveSentUs = timeUs
            let kind = isPencil ? Kind.pencil : Kind.touch
            let pressure = isPencil ? UInt16(clamping: Int(force * 1000)) : 0
            return move(kind: kind, buttons: 1, at: point, timeUs: timeUs, pressure: pressure)
        case .rightDown:
            guard let point, timeUs &- lastMoveSentUs >= Self.moveIntervalUs else { return [] }
            lastMoveSentUs = timeUs
            return move(kind: Kind.touch, buttons: 0b10, at: point, timeUs: timeUs)
        case .scrolling(let last):
            guard let centroid else { return [] }
            // Norm delta → video pixels. Fingers moving down = positive dy =
            // wheel up on the host: the content follows the fingers,
            // direct-manipulation style.
            let dxPx = (CGFloat(Int32(centroid.x) - Int32(last.x)) / 65535) * videoPixelSize.width
            let dyPx = (CGFloat(Int32(centroid.y) - Int32(last.y)) / 65535) * videoPixelSize.height
            scrollRemainder.width += dxPx
            scrollRemainder.height += dyPx
            guard timeUs &- lastMoveSentUs >= Self.moveIntervalUs else { return [] }
            let sendDx = Int16(clamping: Int(scrollRemainder.width.rounded()))
            let sendDy = Int16(clamping: Int(scrollRemainder.height.rounded()))
            guard sendDx != 0 || sendDy != 0 else { return [] }
            scrollRemainder = .zero
            lastMoveSentUs = timeUs
            mode = .scrolling(lastCentroid: centroid)
            return move(kind: Kind.scroll, buttons: 0, at: centroid, timeUs: timeUs,
                        dx: sendDx, dy: sendDy)
        case .idle, .suppressed:
            return []
        }
    }

    mutating func touchEnded(at point: Point?, cancelled: Bool, timeUs: UInt64) -> [Output] {
        touchCount = max(0, touchCount - 1)
        let releasePhase = cancelled ? Phase.cancelled : Phase.ended
        switch mode {
        case .pending(let start, _):
            mode = .idle
            guard !cancelled else { return [] }
            // A clean tap: press + release at the touch-down point.
            return edge(Phase.began, kind: Kind.touch, buttons: 1, at: start, timeUs: timeUs)
                + edge(Phase.ended, kind: Kind.touch, buttons: 1, at: start, timeUs: timeUs)
        case .leftDown(let isPencil):
            mode = .idle
            let kind = isPencil ? Kind.pencil : Kind.touch
            return edge(releasePhase, kind: kind, buttons: 1,
                        at: point ?? Point(x: 32767, y: 32767), timeUs: timeUs)
        case .rightDown:
            mode = .idle
            return edge(releasePhase, kind: Kind.touch, buttons: 0b10,
                        at: point ?? Point(x: 32767, y: 32767), timeUs: timeUs)
        case .scrolling:
            if touchCount == 0 { mode = .idle }
            return []
        case .suppressed:
            if touchCount == 0 { mode = .idle }
            return []
        case .idle:
            return []
        }
    }

    /// The hosting view arms a timer when a touch goes down; firing it while
    /// the touch is still stationary turns the hold into a right click.
    mutating func holdTimerFired(timeUs: UInt64) -> [Output] {
        guard case .pending(let start, let isPencil) = mode, !isPencil,
              timeUs &- pendingSinceUs >= Self.holdRightClickUs else { return [] }
        mode = .rightDown
        return edge(Phase.began, kind: Kind.touch, buttons: 0b10, at: start, timeUs: timeUs)
    }

    /// Three-finger tap: local HUD toggle, never relayed.
    mutating func threeFingerTap(timeUs: UInt64) -> [Output] {
        var outputs = suppress(timeUs: timeUs)
        outputs.append(.toggleHUD)
        return outputs
    }

    private mutating func suppress(timeUs: UInt64) -> [Output] {
        var outputs: [Output] = []
        switch mode {
        case .leftDown(let isPencil):
            outputs = edge(Phase.cancelled, kind: isPencil ? Kind.pencil : Kind.touch,
                           buttons: 1, at: Point(x: 32767, y: 32767), timeUs: timeUs)
        case .rightDown:
            outputs = edge(Phase.cancelled, kind: Kind.touch, buttons: 0b10,
                           at: Point(x: 32767, y: 32767), timeUs: timeUs)
        case .pending, .scrolling, .idle, .suppressed:
            break
        }
        mode = touchCount > 0 ? .suppressed : .idle
        return outputs
    }
}
