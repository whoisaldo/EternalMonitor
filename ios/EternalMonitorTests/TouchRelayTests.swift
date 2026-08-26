import XCTest
@testable import EternalMonitor

final class ContentRectMapperTests: XCTestCase {
    // 16:9 video on a 4:3-ish canvas → full width, letterboxed top/bottom.
    let mapper = ContentRectMapper(
        viewSize: CGSize(width: 1210, height: 834),
        videoSize: CGSize(width: 1920, height: 1080)
    )

    func testContentRectIsLetterboxed() {
        let rect = mapper.contentRect
        XCTAssertEqual(rect.minX, 0, accuracy: 0.01)
        XCTAssertEqual(rect.width, 1210, accuracy: 0.01)
        XCTAssertEqual(rect.height, 1210 * 9 / 16, accuracy: 0.1)
        XCTAssertGreaterThan(rect.minY, 70)
    }

    func testCornersMapToNormalizedExtremes() {
        let rect = mapper.contentRect
        let topLeft = mapper.normalize(CGPoint(x: rect.minX, y: rect.minY))
        XCTAssertEqual(topLeft?.x, 0)
        XCTAssertEqual(topLeft?.y, 0)

        let bottomRight = mapper.normalize(CGPoint(x: rect.maxX, y: rect.maxY))
        XCTAssertEqual(bottomRight?.x, 65535)
        XCTAssertEqual(bottomRight?.y, 65535)

        let center = mapper.normalize(CGPoint(x: rect.midX, y: rect.midY))
        XCTAssertEqual(Double(center?.x ?? 0), 32768, accuracy: 60)
        XCTAssertEqual(Double(center?.y ?? 0), 32768, accuracy: 60)
    }

    func testLetterboxBarsRejectTouches() {
        XCTAssertNil(mapper.normalize(CGPoint(x: 600, y: 5)), "top bar")
        XCTAssertNil(mapper.normalize(CGPoint(x: 600, y: 830)), "bottom bar")
    }

    func testNoVideoMeansNoMapping() {
        let empty = ContentRectMapper(viewSize: CGSize(width: 100, height: 100), videoSize: .zero)
        XCTAssertNil(empty.normalize(CGPoint(x: 50, y: 50)))
    }
}

final class TouchRelayMachineTests: XCTestCase {
    typealias P = TouchRelayMachine.Point
    var machine = TouchRelayMachine()
    var t: UInt64 = 10_000_000

    override func setUp() {
        machine = TouchRelayMachine()
        machine.videoPixelSize = CGSize(width: 1920, height: 1080)
        t = 10_000_000
    }

    private func sends(_ outputs: [TouchRelayMachine.Output]) -> [WireInputEvent] {
        outputs.compactMap {
            if case .send(let event) = $0 { return event }
            return nil
        }
    }

    func testTapClicksAtTouchDownPoint() {
        let start = P(x: 1000, y: 2000)
        XCTAssertTrue(machine.touchBegan(at: start, isPencil: false, timeUs: t).isEmpty,
                      "a bare touch-down must not click yet — it could become a scroll")
        let events = sends(machine.touchEnded(at: start, cancelled: false, timeUs: t + 80_000))
        XCTAssertEqual(events.count, 4, "began ×2 + ended ×2")
        XCTAssertEqual(events[0].phase, TouchRelayMachine.Phase.began)
        XCTAssertEqual(events[0], events[1], "edges are duplicated with one id")
        XCTAssertEqual(events[2].phase, TouchRelayMachine.Phase.ended)
        XCTAssertNotEqual(events[0].eventId, events[2].eventId)
        XCTAssertTrue(events.allSatisfy { $0.xNorm == 1000 && $0.yNorm == 2000 && $0.buttons == 1 })
    }

    func testCancelledPendingTouchDoesNotClick() {
        _ = machine.touchBegan(at: P(x: 100, y: 100), isPencil: false, timeUs: t)
        let outputs = machine.touchEnded(at: P(x: 100, y: 100), cancelled: true, timeUs: t + 50_000)
        XCTAssertTrue(outputs.isEmpty)
    }

    func testDragPressesAtStartAndCoalescesMoves() {
        let start = P(x: 10000, y: 10000)
        _ = machine.touchBegan(at: start, isPencil: false, timeUs: t)

        // Beyond the slop: press at the start point, then catch up.
        let commit = sends(machine.touchMoved(
            to: P(x: 11000, y: 10000), centroid: nil, isPencil: false, force: 0, timeUs: t + 20_000
        ))
        XCTAssertEqual(commit.count, 3)
        XCTAssertEqual(commit[0].phase, TouchRelayMachine.Phase.began)
        XCTAssertEqual(commit[0].xNorm, 10000, "press lands where the finger first touched")
        XCTAssertEqual(commit[2].phase, TouchRelayMachine.Phase.moved)
        XCTAssertEqual(commit[2].xNorm, 11000)

        // A move 1 ms later is inside the 240 Hz budget: dropped.
        XCTAssertTrue(machine.touchMoved(
            to: P(x: 11100, y: 10000), centroid: nil, isPencil: false, force: 0, timeUs: t + 21_000
        ).isEmpty)

        // 5 ms later: relayed.
        let later = sends(machine.touchMoved(
            to: P(x: 12000, y: 10000), centroid: nil, isPencil: false, force: 0, timeUs: t + 26_000
        ))
        XCTAssertEqual(later.count, 1)

        let release = sends(machine.touchEnded(at: P(x: 12000, y: 10000), cancelled: false, timeUs: t + 40_000))
        XCTAssertEqual(release.count, 2)
        XCTAssertEqual(release[0].phase, TouchRelayMachine.Phase.ended)
    }

    func testSmallJitterStaysATap() {
        let start = P(x: 10000, y: 10000)
        _ = machine.touchBegan(at: start, isPencil: false, timeUs: t)
        XCTAssertTrue(machine.touchMoved(
            to: P(x: 10100, y: 10100), centroid: nil, isPencil: false, force: 0, timeUs: t + 10_000
        ).isEmpty, "movement inside the slop must not commit a drag")
        let events = sends(machine.touchEnded(at: start, cancelled: false, timeUs: t + 60_000))
        XCTAssertEqual(events.count, 4, "still a clean tap")
    }

    func testTwoFingerScrollNeverClicks() {
        _ = machine.touchBegan(at: P(x: 30000, y: 30000), isPencil: false, timeUs: t)
        XCTAssertTrue(machine.touchBegan(at: P(x: 34000, y: 30000), isPencil: false, timeUs: t + 5_000).isEmpty)

        // Centroid slides down 655 norm units ≈ 10.8 px of 1080p video.
        let scroll = sends(machine.touchMoved(
            to: P(x: 30000, y: 30655),
            centroid: P(x: 32000, y: 30655),
            isPencil: false, force: 0, timeUs: t + 15_000
        ))
        XCTAssertEqual(scroll.count, 1)
        XCTAssertEqual(scroll[0].kind, TouchRelayMachine.Kind.scroll)
        XCTAssertEqual(scroll[0].scrollDy, 11)
        XCTAssertEqual(scroll[0].buttons, 0)

        XCTAssertTrue(machine.touchEnded(at: nil, cancelled: false, timeUs: t + 30_000).isEmpty)
        XCTAssertTrue(machine.touchEnded(at: nil, cancelled: false, timeUs: t + 31_000).isEmpty)
    }

    func testSecondFingerDuringDragReleasesThenScrolls() {
        _ = machine.touchBegan(at: P(x: 10000, y: 10000), isPencil: false, timeUs: t)
        _ = machine.touchMoved(
            to: P(x: 12000, y: 10000), centroid: nil, isPencil: false, force: 0, timeUs: t + 10_000
        )
        let joined = sends(machine.touchBegan(at: P(x: 12000, y: 12000), isPencil: false, timeUs: t + 20_000))
        XCTAssertEqual(joined.count, 2)
        XCTAssertEqual(joined[0].phase, TouchRelayMachine.Phase.ended, "left button released before scrolling")
    }

    func testLongPressRightClicks() {
        let point = P(x: 40000, y: 40000)
        _ = machine.touchBegan(at: point, isPencil: false, timeUs: t)
        XCTAssertTrue(machine.holdTimerFired(timeUs: t + 100_000).isEmpty,
                      "early timer (re-armed race) must not fire")
        let down = sends(machine.holdTimerFired(timeUs: t + 510_000))
        XCTAssertEqual(down.count, 2)
        XCTAssertEqual(down[0].buttons, 0b10)
        XCTAssertEqual(down[0].phase, TouchRelayMachine.Phase.began)

        let up = sends(machine.touchEnded(at: point, cancelled: false, timeUs: t + 700_000))
        XCTAssertEqual(up.count, 2)
        XCTAssertEqual(up[0].buttons, 0b10)
        XCTAssertEqual(up[0].phase, TouchRelayMachine.Phase.ended)
    }

    func testPencilPressesImmediately() {
        let start = sends(machine.touchBegan(at: P(x: 500, y: 500), isPencil: true, timeUs: t))
        XCTAssertEqual(start.count, 2, "ink cannot wait out tap disambiguation")
        XCTAssertEqual(start[0].kind, TouchRelayMachine.Kind.pencil)
        XCTAssertEqual(start[0].phase, TouchRelayMachine.Phase.began)

        let moved = sends(machine.touchMoved(
            to: P(x: 600, y: 600), centroid: nil, isPencil: true, force: 0.5, timeUs: t + 10_000
        ))
        XCTAssertEqual(moved.count, 1)
        XCTAssertEqual(moved[0].pressureX1000, 500)
    }

    func testThreeFingerTapTogglesHUDWithoutRelaying() {
        _ = machine.touchBegan(at: P(x: 100, y: 100), isPencil: false, timeUs: t)
        _ = machine.touchBegan(at: P(x: 200, y: 200), isPencil: false, timeUs: t)
        _ = machine.touchBegan(at: P(x: 300, y: 300), isPencil: false, timeUs: t + 1_000)
        let outputs = machine.threeFingerTap(timeUs: t + 2_000)
        XCTAssertEqual(outputs, [.toggleHUD], "no wire traffic for the local HUD gesture")
    }

    func testLetterboxTouchDownIsSuppressed() {
        XCTAssertTrue(machine.touchBegan(at: nil, isPencil: false, timeUs: t).isEmpty)
        XCTAssertTrue(machine.touchEnded(at: nil, cancelled: false, timeUs: t + 50_000).isEmpty,
                      "a touch that started on the letterbox must never click")
    }
}
