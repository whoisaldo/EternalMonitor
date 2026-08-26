import SwiftUI
import UIKit

/// Full-screen touch layer that relays gestures to the host PC. Present only
/// when input relay is negotiated; SwiftUI controls above it (HUD, disconnect)
/// still hit-test first.
struct TouchRelayView: UIViewRepresentable {
    @EnvironmentObject var connectionManager: ConnectionManager
    var onToggleHUD: () -> Void

    func makeUIView(context: Context) -> RelayTouchUIView {
        let view = RelayTouchUIView()
        view.onEvent = { [weak connectionManager] event in
            connectionManager?.sendInput(event)
        }
        view.onToggleHUD = onToggleHUD
        return view
    }

    func updateUIView(_ view: RelayTouchUIView, context: Context) {
        view.videoSize = connectionManager.videoSize
        view.onToggleHUD = onToggleHUD
    }
}

final class RelayTouchUIView: UIView {
    var onEvent: ((WireInputEvent) -> Void)?
    var onToggleHUD: (() -> Void)?
    var videoSize: CGSize = .zero {
        didSet { machine.videoPixelSize = videoSize }
    }

    private var machine = TouchRelayMachine()
    private var holdTimer: DispatchWorkItem?
    private var hudTapFired = false

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        backgroundColor = .clear
        // VoiceOver: this surface IS the remote pointer — pass touches
        // straight through instead of narrating them.
        isAccessibilityElement = true
        accessibilityLabel = "Remote control surface. Touches control the PC."
        accessibilityTraits = .allowsDirectInteraction
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    private var mapper: ContentRectMapper {
        ContentRectMapper(viewSize: bounds.size, videoSize: videoSize)
    }

    private func norm(_ touch: UITouch) -> TouchRelayMachine.Point? {
        mapper.normalize(touch.location(in: self)).map {
            TouchRelayMachine.Point(x: $0.x, y: $0.y)
        }
    }

    private func centroid(of event: UIEvent?) -> TouchRelayMachine.Point? {
        guard let touches = event?.allTouches?.filter({
            $0.phase == .began || $0.phase == .moved || $0.phase == .stationary
        }), !touches.isEmpty else { return nil }
        var sum = CGPoint.zero
        for touch in touches {
            let p = touch.location(in: self)
            sum.x += p.x
            sum.y += p.y
        }
        let mean = CGPoint(x: sum.x / CGFloat(touches.count), y: sum.y / CGFloat(touches.count))
        return mapper.normalize(mean).map { TouchRelayMachine.Point(x: $0.x, y: $0.y) }
    }

    private func run(_ outputs: [TouchRelayMachine.Output]) {
        for output in outputs {
            switch output {
            case .send(let event): onEvent?(event)
            case .toggleHUD: onToggleHUD?()
            }
        }
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        let now = ControlChannel.clientNowUs()
        for touch in touches {
            run(machine.touchBegan(
                at: norm(touch),
                isPencil: touch.type == .pencil,
                timeUs: now
            ))
        }

        let activeCount = event?.allTouches?.filter { $0.phase != .ended && $0.phase != .cancelled }.count ?? 0
        if activeCount >= 3 && !hudTapFired {
            hudTapFired = true
            run(machine.threeFingerTap(timeUs: now))
        }

        holdTimer?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.run(self.machine.holdTimerFired(timeUs: ControlChannel.clientNowUs()))
        }
        holdTimer = work
        DispatchQueue.main.asyncAfter(
            deadline: .now() + .microseconds(Int(TouchRelayMachine.holdRightClickUs)),
            execute: work
        )
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let touch = touches.first else { return }
        let force = touch.maximumPossibleForce > 0 ? touch.force / touch.maximumPossibleForce : 0
        run(machine.touchMoved(
            to: norm(touch),
            centroid: centroid(of: event),
            isPencil: touch.type == .pencil,
            force: force,
            timeUs: ControlChannel.clientNowUs()
        ))
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        finish(touches, with: event, cancelled: false)
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        finish(touches, with: event, cancelled: true)
    }

    private func finish(_ touches: Set<UITouch>, with event: UIEvent?, cancelled: Bool) {
        let now = ControlChannel.clientNowUs()
        for touch in touches {
            run(machine.touchEnded(at: norm(touch), cancelled: cancelled, timeUs: now))
        }
        let remaining = event?.allTouches?.filter { $0.phase != .ended && $0.phase != .cancelled }.count ?? 0
        if remaining == 0 {
            holdTimer?.cancel()
            holdTimer = nil
            hudTapFired = false
        }
    }
}
