import XCTest
@testable import EternalMonitor

final class ClockEstimatorTests: XCTestCase {
    /// Host clock = client clock + 5s, symmetric 10ms network legs.
    private func symmetricExchange(at clientTime: UInt64, offset: Int64, legUs: UInt64, hostProcessing: UInt64 = 100)
        -> (t1: UInt64, t2: UInt64, t3: UInt64, t4: UInt64)
    {
        let t1 = clientTime
        let t2 = UInt64(Int64(t1 + legUs) + offset)
        let t3 = t2 + hostProcessing
        let t4 = t1 + legUs + hostProcessing + legUs
        return (t1, t2, t3, t4)
    }

    func testOffsetFromSymmetricExchanges() {
        var estimator = ClockEstimator()
        let trueOffset: Int64 = 5_000_000
        for i in 0..<3 {
            let e = symmetricExchange(at: UInt64(1_000_000 * (i + 1)), offset: trueOffset, legUs: 10_000)
            estimator.addExchange(t1: e.t1, t2: e.t2, t3: e.t3, t4: e.t4)
        }
        let measured = try! XCTUnwrap(estimator.offsetUs)
        XCTAssertEqual(measured, trueOffset, accuracy: 1000)
        XCTAssertEqual(try! XCTUnwrap(estimator.rttUs), 20_000, accuracy: 500)
    }

    func testNeedsMinimumSamples() {
        var estimator = ClockEstimator()
        let e = symmetricExchange(at: 1_000_000, offset: 0, legUs: 5_000)
        estimator.addExchange(t1: e.t1, t2: e.t2, t3: e.t3, t4: e.t4)
        XCTAssertNil(estimator.offsetUs)
    }

    func testMinRTTSampleWins() {
        var estimator = ClockEstimator()
        let trueOffset: Int64 = -2_000_000

        // Two congested exchanges with asymmetric delay (corrupted offsets)...
        // Client times start well above |offset| so host time stays positive.
        for (i, legs) in [(40_000 as UInt64, 5_000 as UInt64), (3_000, 60_000)].enumerated() {
            let t1 = UInt64(10_000_000 * (i + 1))
            let t2 = UInt64(Int64(t1 + legs.0) + trueOffset)
            let t3 = t2 + 100
            let t4 = t1 + legs.0 + 100 + legs.1
            estimator.addExchange(t1: t1, t2: t2, t3: t3, t4: t4)
        }
        // ...and one clean symmetric exchange.
        let clean = symmetricExchange(at: 30_000_000, offset: trueOffset, legUs: 2_000)
        estimator.addExchange(t1: clean.t1, t2: clean.t2, t3: clean.t3, t4: clean.t4)

        let measured = try! XCTUnwrap(estimator.offsetUs)
        XCTAssertEqual(measured, trueOffset, accuracy: 500, "the min-RTT sample's offset must win")
    }

    func testEndToEndLatency() {
        var estimator = ClockEstimator()
        let offset: Int64 = 1_000_000 // host is 1s ahead
        for i in 0..<3 {
            let e = symmetricExchange(at: UInt64(1_000_000 * (i + 1)), offset: offset, legUs: 1_000)
            estimator.addExchange(t1: e.t1, t2: e.t2, t3: e.t3, t4: e.t4)
        }
        // Host captured at host-time 10.000s = client-time 9.000s;
        // client observes the decoded frame at client-time 9.045s -> 45ms e2e.
        let latency = estimator.endToEndLatencyUs(
            hostCaptureUs: 10_000_000,
            clientNowUs: 9_045_000
        )
        XCTAssertEqual(Double(try! XCTUnwrap(latency)), 45_000, accuracy: 1500)
    }

    func testResetClearsState() {
        var estimator = ClockEstimator()
        for i in 0..<3 {
            let e = symmetricExchange(at: UInt64(1_000_000 * (i + 1)), offset: 0, legUs: 1_000)
            estimator.addExchange(t1: e.t1, t2: e.t2, t3: e.t3, t4: e.t4)
        }
        XCTAssertNotNil(estimator.offsetUs)
        estimator.reset()
        XCTAssertNil(estimator.offsetUs)
    }
}
