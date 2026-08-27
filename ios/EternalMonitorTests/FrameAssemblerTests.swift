import XCTest
@testable import EternalMonitor

final class FrameAssemblerTests: XCTestCase {
    private var assembler = FrameAssembler()
    private var completed: [Data] = []
    private var diagnostics: [String] = []

    override func setUp() {
        super.setUp()
        assembler = FrameAssembler()
        completed = []
        diagnostics = []
        assembler.onFrameAssembled = { [weak self] data, _, _, _ in self?.completed.append(data) }
        assembler.onDiagnostic = { [weak self] message in self?.diagnostics.append(message) }
    }

    private func add(seq: UInt32, index: UInt16, count: UInt16, epoch: UInt32 = 1, byte: UInt8) {
        assembler.addFragment(
            seq: seq, index: index, count: count, epoch: epoch,
            isKeyframe: false, captureTimestampUs: 0, payload: Data([byte])
        )
    }

    func testAssemblesInOrderAndOutOfOrder() {
        add(seq: 1, index: 0, count: 3, byte: 0xA)
        add(seq: 1, index: 2, count: 3, byte: 0xC)
        add(seq: 1, index: 1, count: 3, byte: 0xB)
        XCTAssertEqual(completed, [Data([0xA, 0xB, 0xC])])

        add(seq: 2, index: 0, count: 1, byte: 0xD)
        XCTAssertEqual(completed.last, Data([0xD]))
    }

    func testDuplicateOfCompletedFrameIsIgnored() {
        add(seq: 1, index: 0, count: 1, byte: 0xA)
        add(seq: 1, index: 0, count: 1, byte: 0xA)
        XCTAssertEqual(completed.count, 1)
    }

    func testStaleSeqDroppedButBigBackjumpResets() {
        add(seq: 500, index: 0, count: 1, byte: 0xA)
        add(seq: 499, index: 0, count: 1, byte: 0xB)
        XCTAssertEqual(completed.count, 1, "stale frame within the gap must be dropped")

        add(seq: 1, index: 0, count: 1, byte: 0xC)
        XCTAssertEqual(completed.count, 2, "seq back-jump beyond the gap is a stream restart")
        XCTAssertEqual(completed.last, Data([0xC]))
    }

    func testHigherEpochResetsLowerEpochDropped() {
        add(seq: 900, index: 0, count: 1, epoch: 5, byte: 0xA)
        add(seq: 1, index: 0, count: 1, epoch: 6, byte: 0xB)
        XCTAssertEqual(completed.count, 2, "new epoch must reset and accept small seq")

        add(seq: 901, index: 0, count: 1, epoch: 5, byte: 0xC)
        XCTAssertEqual(completed.count, 2, "stale-epoch fragment must be dropped")
    }

    func testAcceptsFramesUpToTheProtocolFragmentCap() {
        // A 1440p/4K scene-change IDR runs past 1024 fragments. The host
        // fragments against the protocol cap and the wire parser accepts up to
        // it, so an assembler cap below that silently dropped every fragment
        // of such a frame — and with no sync sample the decoder then discarded
        // everything after it too.
        XCTAssertEqual(
            FrameAssembler.maxFragmentCount, MediaHeader.maxFragCount,
            "the assembler cap must match the protocol cap the host sends against"
        )

        let count: UInt16 = 2000
        for index in 0..<count {
            add(seq: 1, index: index, count: count, byte: 0xA)
        }
        XCTAssertEqual(completed.count, 1, "a large but legal frame must assemble")
        XCTAssertEqual(completed.first?.count, Int(count))
    }

    func testOneBogusEpochCannotStrandTheStream() {
        add(seq: 10, index: 0, count: 1, epoch: 5, byte: 0xA)
        XCTAssertEqual(completed.count, 1)

        // One corrupted or spoofed fragment claiming the maximum epoch.
        add(seq: 11, index: 0, count: 1, epoch: .max, byte: 0xFF)
        let afterPoison = completed.count

        // The real stream is now "stale" against an epoch the host can never
        // reach. It must not stay that way for the rest of the session.
        for i in 0..<UInt32(600) {
            add(seq: 100 + i, index: 0, count: 1, epoch: 5, byte: 0xB)
        }
        XCTAssertGreaterThan(
            completed.count, afterPoison,
            "the assembler must re-sync to the live stream instead of freezing forever"
        )

        // And it keeps flowing afterwards.
        let beforeTail = completed.count
        add(seq: 900, index: 0, count: 1, epoch: 5, byte: 0xC)
        XCTAssertEqual(completed.count, beforeTail + 1)
        XCTAssertEqual(completed.last, Data([0xC]))
    }

    func testInterleavedStragglersNeverTripTheResync() {
        add(seq: 1, index: 0, count: 1, epoch: 7, byte: 0xA)

        // A real restart: epoch 8 is live while epoch-7 stragglers keep
        // arriving. Far more stale drops than the resync threshold, but the
        // accepted fragments in between must keep resetting the streak.
        for i in 0..<UInt32(1200) {
            add(seq: 1000 + i, index: 0, count: 1, epoch: 8, byte: 0xB)
            add(seq: 500 + i, index: 0, count: 1, epoch: 7, byte: 0xC)
        }
        XCTAssertFalse(
            completed.contains(Data([0xC])),
            "old-run stragglers must never be accepted while the new run is live"
        )
    }

    func testCompletionEvictsOlderPartials() {
        add(seq: 1, index: 0, count: 2, byte: 0xA)
        add(seq: 2, index: 0, count: 1, byte: 0xB)
        XCTAssertEqual(completed, [Data([0xB])])

        // The evicted frame's late fragment is now stale.
        add(seq: 1, index: 1, count: 2, byte: 0xC)
        XCTAssertEqual(completed.count, 1)
    }

    func testMismatchedFragmentCountDoesNotWipeProgress() {
        add(seq: 1, index: 0, count: 3, byte: 0xA)
        add(seq: 1, index: 1, count: 2, byte: 0xB) // conflicting count — ignored
        XCTAssertTrue(diagnostics.contains { $0.contains("mismatched count") })

        add(seq: 1, index: 1, count: 3, byte: 0xB)
        add(seq: 1, index: 2, count: 3, byte: 0xC)
        XCTAssertEqual(completed, [Data([0xA, 0xB, 0xC])], "first-seen count must win")
    }

    func testInvalidFragmentsRejected() {
        add(seq: 1, index: 0, count: 0, byte: 0xA)
        add(seq: 1, index: 2, count: 2, byte: 0xA)
        XCTAssertTrue(completed.isEmpty)
        XCTAssertEqual(diagnostics.count, 2)
    }

    func testFragmentCountAboveCapRejected() {
        add(seq: 1, index: 0, count: FrameAssembler.maxFragmentCount + 1, byte: 0xA)
        XCTAssertTrue(completed.isEmpty)
        XCTAssertTrue(diagnostics.contains { $0.contains("exceeds cap") })
    }

    func testPendingFrameCapDropsOldestFirst() {
        // Fill the pending table with partial frames.
        for seq in 1...UInt32(FrameAssembler.maxPendingFrames) {
            add(seq: seq, index: 0, count: 2, byte: UInt8(seq))
        }
        // One more forces the oldest (seq 1) out.
        add(seq: 100, index: 0, count: 2, byte: 0x64)
        XCTAssertTrue(diagnostics.contains { $0.contains("capacity") })

        // seq 1 can no longer complete…
        add(seq: 1, index: 1, count: 2, byte: 0x01)
        XCTAssertTrue(completed.isEmpty)

        // …but seq 100 still can.
        add(seq: 100, index: 1, count: 2, byte: 0x65)
        XCTAssertEqual(completed, [Data([0x64, 0x65])])
    }

    func testResetClearsEpochAndState() {
        add(seq: 5, index: 0, count: 2, epoch: 9, byte: 0xA)
        assembler.reset()
        add(seq: 1, index: 0, count: 1, epoch: 2, byte: 0xB)
        XCTAssertEqual(completed, [Data([0xB])], "any epoch must be accepted after reset")
    }
}
