import Foundation
import VideoToolbox
import CoreMedia
import os

/// Verbose per-frame decode logging. Off by default — these prints run on the decode hot path
/// (every submitted access unit and every decoded frame) and cause noticeable jank at 60 fps.
/// Flip to `true` only when debugging the decode pipeline.
private let verboseDecodeLogging = false

/// H.264 hardware decoder using VideoToolbox.
/// Parses NAL units, extracts SPS/PPS, converts Annex B → AVCC, decodes to CVPixelBuffer (NV12).
///
/// Session lifecycle: ONE `VTDecompressionSession` per format description. When new SPS/PPS
/// arrive, an unchanged format is ignored, a compatible change is adopted in place
/// (`VTDecompressionSessionCanAcceptFormatDescription`), and only a genuinely incompatible
/// change tears the session down. The old per-IDR session recreation (the "AMD recovery
/// hammer") is gone — the host prepends fresh SPS/PPS on every IDR, so the format-change
/// path covers resync without rebuilding the hardware decoder twice a second.
final class VideoDecoder {
    var onFrameDecoded: ((_ pixelBuffer: CVPixelBuffer, _ timestampUs: UInt64) -> Void)?
    var onEvent: ((String) -> Void)?
    /// The decoder cannot make progress until the next keyframe (session died, decode error).
    /// Protocol v2 turns this into a keyframe request to the host; today it is diagnostic.
    var onNeedsKeyframe: (() -> Void)?

    /// Which bitstream the host is sending. Sniffed from the NAL units
    /// themselves (an HEVC VPS or an H.264 SPS in a keyframe) because the
    /// host switches codecs via a live encoder reopen — the bitstream is the
    /// authority, not the last STREAM_CONFIG to arrive.
    enum StreamCodec { case h264, hevc }

    private var formatDescription: CMVideoFormatDescription?
    private var decompressionSession: VTDecompressionSession?
    private var codec: StreamCodec = .h264
    private var vps: Data?
    private var sps: Data?
    private var pps: Data?
    private var loggedPacketizations = Set<String>()
    private var loggedNALTypes = Set<UInt8>()
    private var hasLoggedEmptyPayload = false
    private var hasLoggedFirstPacketPrefix = false
    private var hasLoggedFirstNALPrefix = false
    private var hasLoggedFirstPacketHex = false
    private var waitingForSyncSample = true
    private var isShutdown = false
    private var packetLogCounter: UInt64 = 0

    private let decodeQueue = DispatchQueue(label: "com.eternal.decode", qos: .userInteractive)

    deinit {
        // `shutdown()` is the proper teardown; this is the safety net if an owner
        // drops the decoder without calling it. No callbacks can be in flight at
        // deinit (the session retains its output handlers' captures weakly).
        if let session = decompressionSession {
            VTDecompressionSessionInvalidate(session)
        }
    }

    func decode(packet: FramePacket) {
        decodeQueue.async { [weak self] in
            self?.decodeOnQueue(packet: packet)
        }
    }

    /// Tear down on the decode queue, provably after any in-flight decode call.
    /// Captures `self` strongly so the session cannot be freed under a live callback;
    /// `isShutdown` makes any decode enqueued after this a no-op.
    func shutdown(completion: (() -> Void)? = nil) {
        decodeQueue.async {
            self.isShutdown = true
            self.teardownSession(waitForFrames: true)
            self.formatDescription = nil
            self.codec = .h264
            self.vps = nil
            self.sps = nil
            self.pps = nil
            self.waitingForSyncSample = true
            self.loggedPacketizations.removeAll()
            self.loggedNALTypes.removeAll()
            self.hasLoggedEmptyPayload = false
            self.hasLoggedFirstPacketPrefix = false
            self.hasLoggedFirstNALPrefix = false
            self.hasLoggedFirstPacketHex = false
            self.packetLogCounter = 0
            completion?()
        }
    }

    // MARK: - Internal

    private func teardownSession(waitForFrames: Bool) {
        guard let session = decompressionSession else { return }
        if waitForFrames {
            VTDecompressionSessionWaitForAsynchronousFrames(session)
        }
        VTDecompressionSessionInvalidate(session)
        decompressionSession = nil
    }

    private func decodeOnQueue(packet: FramePacket) {
        guard !isShutdown else { return }
        if verboseDecodeLogging && !hasLoggedFirstPacketHex {
            hasLoggedFirstPacketHex = true
            let hex = packet.data.prefix(16).map { String(format: "%02X", $0) }.joined(separator: " ")
            print("[VideoDecoder] First packet hex (\(packet.data.count) bytes): \(hex)")
        }
        let parsed = parseNALUnits(from: packet.data)
        logPacketPrefixIfNeeded(packet.data)
        if loggedPacketizations.insert(parsed.packetization).inserted {
            onEvent?("Detected \(parsed.packetization) H.264 packetization")
        }

        let nalUnits = parsed.units
        guard !nalUnits.isEmpty else {
            if let parameterSets = parseAVCCConfigurationRecord(packet.data) {
                sps = parameterSets.sps
                pps = parameterSets.pps
                onEvent?(
                    "Parsed AVCC configuration record SPS=\(parameterSets.sps.count)B PPS=\(parameterSets.pps.count)B"
                )
                tryCreateFormatDescription()
                return
            }
            if !hasLoggedEmptyPayload {
                onEvent?("No H.264 NAL units parsed from packet bytes=\(packet.data.count)")
                hasLoggedEmptyPayload = true
            }
            return
        }
        hasLoggedEmptyPayload = false

        logFirstNALPrefixIfNeeded(nalUnits[0])

        if let detected = Self.sniffCodec(nalUnits), detected != codec {
            onEvent?("Bitstream codec switched to \(detected == .hevc ? "HEVC" : "H.264")")
            codec = detected
            vps = nil
            sps = nil
            pps = nil
            waitingForSyncSample = true
        }
        if codec == .hevc {
            decodeHEVCNALs(nalUnits, timestampUs: packet.timestampUs)
            return
        }

        var accessUnitNALs: [Data] = []
        var hasSliceNAL = false
        var nalTypesInPacket: [UInt8] = []
        for nal in nalUnits {
            let nalType = nal[0] & 0x1F
            nalTypesInPacket.append(nalType)
            logNALTypeIfNeeded(nalType)

            switch nalType {
            case 6, 9:
                // Strip SEI(6) and AUD(9) — VideoToolbox on some iPad models trips on AUDs and
                // SEIs in the submitted sample buffer, particularly around format-description
                // re-creation. They carry no decode info.
                continue
            case 7: // SPS
                sps = nal
                tryCreateFormatDescription()
            case 8: // PPS
                pps = nal
                tryCreateFormatDescription()
            case 1, 5: // Non-IDR slice, IDR slice
                hasSliceNAL = true
                accessUnitNALs.append(nal)
            default:
                accessUnitNALs.append(nal)
            }
        }

        if hasSliceNAL {
            let isSyncSample = isRandomAccessAccessUnit(accessUnitNALs)
            packetLogCounter += 1
            if verboseDecodeLogging && (packetLogCounter <= 20 || packetLogCounter % 300 == 0) {
                let typesString = nalTypesInPacket.map { String($0) }.joined(separator: ",")
                print(
                    "[VT] pkt seq=\(packet.seq) isKey=\(isSyncSample) nalCount=\(accessUnitNALs.count) nalTypes=\(typesString)"
                )
            }
            decodeAccessUnit(accessUnitNALs, timestampUs: packet.timestampUs, isSyncSample: isSyncSample)
        }
    }

    // MARK: - HEVC

    /// The codec whose parameter sets appear in this access unit, if any.
    /// Exactly `0x40` (HEVC VPS, layer 0) marks HEVC — the byte reads as NAL
    /// type 0 in H.264, which no encoder emits. NOT `0x41`: that is a common
    /// H.264 P-slice (type 1, ref_idc 2) that merely shares the VPS type bits.
    /// `x & 0x9F == 0x07` is a classic H.264 SPS. The host guarantees a VPS on
    /// every HEVC switch keyframe, so VPS-only detection is sufficient.
    static func sniffCodec(_ nalUnits: [Data]) -> StreamCodec? {
        for nal in nalUnits {
            guard let first = nal.first else { continue }
            if first == 0x40 { return .hevc }
            if first & 0x9F == 0x07 { return .h264 }
        }
        return nil
    }

    private func decodeHEVCNALs(_ nalUnits: [Data], timestampUs: UInt64) {
        var accessUnitNALs: [Data] = []
        var hasSliceNAL = false
        var isSyncSample = false
        for nal in nalUnits {
            guard let first = nal.first else { continue }
            let nalType = (first >> 1) & 0x3F
            switch nalType {
            case 32:
                vps = nal
                tryCreateHEVCFormatDescription()
            case 33:
                sps = nal
                tryCreateHEVCFormatDescription()
            case 34:
                pps = nal
                tryCreateHEVCFormatDescription()
            case 35, 39, 40:
                // AUD and SEI — same VideoToolbox hygiene as the H.264 path.
                continue
            default:
                if nalType <= 31 {
                    hasSliceNAL = true
                    if (16...21).contains(nalType) {
                        isSyncSample = true // IRAP: BLA/IDR/CRA
                    }
                }
                accessUnitNALs.append(nal)
            }
        }
        if hasSliceNAL {
            decodeAccessUnit(accessUnitNALs, timestampUs: timestampUs, isSyncSample: isSyncSample)
        }
    }

    private func tryCreateHEVCFormatDescription() {
        guard let vps, let sps, let pps else { return }
        let parameterSets: [Data] = [vps, sps, pps]
        var newFormat: CMFormatDescription?
        let status = parameterSets.withUnsafeBufferPointers { pointers, sizes in
            CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                allocator: kCFAllocatorDefault,
                parameterSetCount: 3,
                parameterSetPointers: pointers,
                parameterSetSizes: sizes,
                nalUnitHeaderLength: 4,
                extensions: nil,
                formatDescriptionOut: &newFormat
            )
        }
        guard status == noErr, let newFormat else {
            onEvent?("Failed to create HEVC format description status=\(status)")
            return
        }
        adoptFormatDescription(
            newFormat,
            describing: "VPS=\(vps.count)B SPS=\(sps.count)B PPS=\(pps.count)B"
        )
    }

    // MARK: - NAL unit parsing (Annex B → individual NAL units)

    private func parseNALUnits(from data: Data) -> (packetization: String, units: [Data]) {
        let annexBUnits = parseAnnexBNALUnits(from: data)
        if !annexBUnits.isEmpty {
            return ("AnnexB", annexBUnits)
        }

        for lengthFieldBytes in [4, 2, 1] {
            if let units = parseLengthPrefixedNALUnits(from: data, lengthFieldBytes: lengthFieldBytes) {
                return ("AVCC(len=\(lengthFieldBytes))", units)
            }
        }

        return ("Unknown", [])
    }

    private func parseAnnexBNALUnits(from data: Data) -> [Data] {
        var units: [Data] = []
        let count = data.count

        data.withUnsafeBytes { raw in
            let bytes = raw.bindMemory(to: UInt8.self)

            guard let first = findStartCode(in: bytes, count: count, from: 0) else { return }
            var i = first.offset + first.length

            while i < count {
                if let next = findStartCode(in: bytes, count: count, from: i) {
                    let nalData = Data(bytes: raw.baseAddress! + i, count: next.offset - i).trimmingTrailingZeros()
                    if !nalData.isEmpty { units.append(nalData) }
                    i = next.offset + next.length
                } else {
                    let nalData = Data(bytes: raw.baseAddress! + i, count: count - i).trimmingTrailingZeros()
                    if !nalData.isEmpty { units.append(nalData) }
                    break
                }
            }
        }

        return units
    }

    private func parseLengthPrefixedNALUnits(from data: Data, lengthFieldBytes: Int) -> [Data]? {
        guard (1...4).contains(lengthFieldBytes), data.count >= lengthFieldBytes else { return nil }

        var units: [Data] = []
        var cursor = 0
        while cursor + lengthFieldBytes <= data.count {
            let nalLength = readBigEndianLength(from: data, offset: cursor, width: lengthFieldBytes)
            cursor += lengthFieldBytes

            guard nalLength > 0, cursor + nalLength <= data.count else { return nil }
            units.append(data.subdata(in: cursor..<(cursor + nalLength)))
            cursor += nalLength
        }

        return cursor == data.count && !units.isEmpty ? units : nil
    }

    private func logNALTypeIfNeeded(_ nalType: UInt8) {
        guard loggedNALTypes.insert(nalType).inserted else { return }
        onEvent?("First observed NAL type \(nalType) (\(describeNALType(nalType)))")
    }

    private func describeNALType(_ nalType: UInt8) -> String {
        switch nalType {
        case 1:
            return "non-IDR slice"
        case 5:
            return "IDR slice"
        case 6:
            return "SEI"
        case 7:
            return "SPS"
        case 8:
            return "PPS"
        case 9:
            return "AUD"
        default:
            return "other"
        }
    }

    // MARK: - Format description

    private func tryCreateFormatDescription() {
        guard let sps, let pps else { return }

        let parameterSets: [Data] = [sps, pps]
        var newFormat: CMFormatDescription?

        let status = parameterSets.withUnsafeBufferPointers { pointers, sizes in
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                allocator: kCFAllocatorDefault,
                parameterSetCount: 2,
                parameterSetPointers: pointers,
                parameterSetSizes: sizes,
                nalUnitHeaderLength: 4,
                formatDescriptionOut: &newFormat
            )
        }

        guard status == noErr, let newFormat else {
            print("[VideoDecoder] Failed to create format description: \(status)")
            onEvent?("Failed to create H.264 format description status=\(status)")
            return
        }

        adoptFormatDescription(newFormat, describing: "SPS=\(sps.count)B PPS=\(pps.count)B")
    }

    private func adoptFormatDescription(_ newFormat: CMFormatDescription, describing detail: String) {
        if let existing = formatDescription,
           CMFormatDescriptionEqual(existing, otherFormatDescription: newFormat) {
            return
        }

        formatDescription = newFormat
        onEvent?("Updated format description \(detail)")

        // Adopt a compatible format change without rebuilding the hardware decoder;
        // only an incompatible change (e.g. resolution switch) costs a session rebuild.
        if let session = decompressionSession,
           VTDecompressionSessionCanAcceptFormatDescription(session, formatDescription: newFormat) {
            onEvent?("Format change accepted by the live session — no rebuild needed")
        } else {
            createDecompressionSession()
        }
    }

    // MARK: - Decompression session

    private func createDecompressionSession() {
        teardownSession(waitForFrames: false)

        guard let formatDescription else { return }

        let outputAttributes: [String: Any] = [
            // NV12: what hardware H.264 decoders natively emit. BGRA forced a
            // VideoToolbox-internal conversion pass and 2.7x the memory bandwidth.
            kCVPixelBufferPixelFormatTypeKey as String:
                kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelBufferMetalCompatibilityKey as String: true,
            kCVPixelBufferIOSurfacePropertiesKey as String: [:] as [String: Any],
        ]

        var session: VTDecompressionSession?
        let status = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            formatDescription: formatDescription,
            decoderSpecification: [
                // Enable (not Require): hardware on device, silent software fallback in
                // the simulator — which is what makes the automated E2E possible at all.
                kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: true
            ] as CFDictionary,
            imageBufferAttributes: outputAttributes as CFDictionary,
            outputCallback: nil,
            decompressionSessionOut: &session
        )

        guard status == noErr, let session else {
            print("[VideoDecoder] Failed to create decompression session: \(status)")
            onEvent?("Failed to create VideoToolbox session status=\(status)")
            return
        }

        VTSessionSetProperty(session, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue!)

        var usingHardware: CFTypeRef?
        VTSessionCopyProperty(
            session,
            key: kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
            allocator: kCFAllocatorDefault,
            valueOut: &usingHardware
        )
        let hardware = (usingHardware as? Bool) == true
        decompressionSession = session
        waitingForSyncSample = true
        onEvent?("VideoToolbox session ready (\(hardware ? "hardware" : "software") decoder)")
        if E2E.enabled {
            E2E.logger.log("E2E_DECODER kind=\(hardware ? "hw" : "sw", privacy: .public)")
        }
    }

    // MARK: - Decode an access unit

    private func decodeAccessUnit(_ nalUnits: [Data], timestampUs: UInt64, isSyncSample: Bool) {
        guard let session = decompressionSession, let formatDescription else {
            let totalBytes = nalUnits.reduce(0) { $0 + $1.count }
            onEvent?(
                "Dropped access unit nalCount=\(nalUnits.count) bytes=\(totalBytes) timestampUs=\(timestampUs) because decoder is not ready"
            )
            return
        }

        let totalBytes = nalUnits.reduce(0) { $0 + $1.count }
        if waitingForSyncSample && !isSyncSample {
            onEvent?(
                "Dropped inter access unit nalCount=\(nalUnits.count) bytes=\(totalBytes) timestampUs=\(timestampUs) while waiting for sync sample"
            )
            return
        }
        if isSyncSample {
            waitingForSyncSample = false
        }

        // VideoToolbox expects a full access unit as consecutive AVCC length-prefixed NALs.
        var avccData = Data()
        avccData.reserveCapacity(totalBytes + (nalUnits.count * 4))
        for nalData in nalUnits {
            var nalLength = UInt32(nalData.count).bigEndian
            avccData.append(Data(bytes: &nalLength, count: 4))
            avccData.append(nalData)
        }

        // Create CMBlockBuffer
        var blockBuffer: CMBlockBuffer?
        let totalLen = avccData.count
        avccData.withUnsafeMutableBytes { rawBuffer in
            guard let ptr = rawBuffer.baseAddress else { return }
            CMBlockBufferCreateWithMemoryBlock(
                allocator: kCFAllocatorDefault,
                memoryBlock: nil,
                blockLength: totalLen,
                blockAllocator: kCFAllocatorDefault,
                customBlockSource: nil,
                offsetToData: 0,
                dataLength: totalLen,
                flags: 0,
                blockBufferOut: &blockBuffer
            )
            if let blockBuffer {
                CMBlockBufferReplaceDataBytes(
                    with: ptr,
                    blockBuffer: blockBuffer,
                    offsetIntoDestination: 0,
                    dataLength: totalLen
                )
            }
        }

        guard let blockBuffer else {
            onEvent?("Failed to allocate block buffer for access unit bytes=\(totalLen)")
            return
        }

        // Create CMSampleBuffer
        var sampleBuffer: CMSampleBuffer?
        var sampleSize = totalLen
        CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault,
            dataBuffer: blockBuffer,
            formatDescription: formatDescription,
            sampleCount: 1,
            sampleTimingEntryCount: 0,
            sampleTimingArray: nil,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &sampleSize,
            sampleBufferOut: &sampleBuffer
        )

        guard let sampleBuffer else {
            onEvent?("Failed to create sample buffer for access unit bytes=\(totalLen)")
            return
        }

        if let attachmentsArray = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: true) {
            let attachments = unsafeBitCast(
                CFArrayGetValueAtIndex(attachmentsArray, 0),
                to: CFMutableDictionary.self
            )
            let syncFlag = isSyncSample ? kCFBooleanFalse! : kCFBooleanTrue!
            CFDictionarySetValue(
                attachments,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_NotSync).toOpaque(),
                Unmanaged.passUnretained(syncFlag).toOpaque()
            )
            CFDictionarySetValue(
                attachments,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_DependsOnOthers).toOpaque(),
                Unmanaged.passUnretained(syncFlag).toOpaque()
            )
        }

        if verboseDecodeLogging {
            let nalCount = nalUnits.count
            print("[VT] Submitting sample: size=\(totalLen) isKeyframe=\(isSyncSample) nalCount=\(nalCount)")
        }

        var infoFlags = VTDecodeInfoFlags()
        let status = VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: sampleBuffer,
            flags: [._EnableAsynchronousDecompression],
            infoFlagsOut: &infoFlags
        ) { [weak self] status, _, imageBuffer, _, _ in
            guard let self else { return }
            if verboseDecodeLogging {
                print("[VT] Output callback fired: status=\(status)")
            }
            guard status == noErr, let pixelBuffer = imageBuffer else {
                self.onEvent?("Decoder callback status=\(status) imageBufferMissing=\(imageBuffer == nil)")
                return
            }
            self.onFrameDecoded?(pixelBuffer, timestampUs)
        }

        if status == kVTInvalidSessionErr {
            // The session died underneath us (typical after app backgrounding).
            // Rebuild it and hold for the next keyframe.
            onEvent?("VideoToolbox session invalidated — recreating and waiting for a keyframe")
            createDecompressionSession()
            waitingForSyncSample = true
            onNeedsKeyframe?()
        } else if status != noErr {
            onEvent?("VTDecodeFrame failed status=\(status) nalCount=\(nalUnits.count) bytes=\(totalLen)")
        }
    }

    private func isRandomAccessAccessUnit(_ nalUnits: [Data]) -> Bool {
        var sawVCL = false

        for nal in nalUnits where !nal.isEmpty {
            let nalType = nal[0] & 0x1F
            switch nalType {
            case 5:
                return true
            case 1...4:
                sawVCL = true
                guard let sliceKind = sliceKind(for: nal), sliceKind.isIntra else {
                    return false
                }
            default:
                break
            }
        }

        return sawVCL
    }

    private func sliceKind(for nal: Data) -> H264SliceKind? {
        guard let nalHeader = nal.first else { return nil }
        let nalType = nalHeader & 0x1F
        switch nalType {
        case 5:
            return .i
        case 1...4:
            let rbsp = rbspPayload(from: nal.dropFirst())
            var reader = H264BitReader(bytes: rbsp)
            guard reader.readUE() != nil else { return nil }
            guard let sliceType = reader.readUE() else { return nil }
            switch sliceType % 5 {
            case 0:
                return .p
            case 1:
                return .b
            case 2:
                return .i
            case 3:
                return .sp
            case 4:
                return .si
            default:
                return nil
            }
        default:
            return nil
        }
    }

    private func rbspPayload<S: Sequence>(from bytes: S) -> [UInt8] where S.Element == UInt8 {
        var rbsp: [UInt8] = []
        rbsp.reserveCapacity(16)
        var zeroCount = 0

        for byte in bytes {
            if zeroCount >= 2 && byte == 0x03 {
                zeroCount = 0
                continue
            }

            rbsp.append(byte)
            if byte == 0 {
                zeroCount += 1
            } else {
                zeroCount = 0
            }
        }

        return rbsp
    }

    private func parseAVCCConfigurationRecord(_ data: Data) -> (sps: Data, pps: Data)? {
        guard data.count >= 7, data.first == 1 else { return nil }

        var cursor = 5
        let spsCount = Int(data[cursor] & 0x1F)
        cursor += 1

        var sps: Data?
        for _ in 0..<spsCount {
            guard cursor + 2 <= data.count else { return nil }
            let length = readBigEndianLength(from: data, offset: cursor, width: 2)
            cursor += 2
            guard cursor + length <= data.count else { return nil }
            if sps == nil {
                sps = data.subdata(in: cursor..<(cursor + length))
            }
            cursor += length
        }

        guard cursor < data.count else { return nil }
        let ppsCount = Int(data[cursor])
        cursor += 1

        var pps: Data?
        for _ in 0..<ppsCount {
            guard cursor + 2 <= data.count else { return nil }
            let length = readBigEndianLength(from: data, offset: cursor, width: 2)
            cursor += 2
            guard cursor + length <= data.count else { return nil }
            if pps == nil {
                pps = data.subdata(in: cursor..<(cursor + length))
            }
            cursor += length
        }

        guard let sps, let pps else { return nil }
        return (sps, pps)
    }

    private func logPacketPrefixIfNeeded(_ data: Data) {
        guard !hasLoggedFirstPacketPrefix else { return }
        hasLoggedFirstPacketPrefix = true
        onEvent?("First packet prefix \(hexPrefix(data, maxBytes: 16))")
    }

    private func logFirstNALPrefixIfNeeded(_ nal: Data) {
        guard !hasLoggedFirstNALPrefix else { return }
        hasLoggedFirstNALPrefix = true
        onEvent?("First NAL prefix \(hexPrefix(nal, maxBytes: 16))")
    }

    private func hexPrefix(_ data: Data, maxBytes: Int) -> String {
        data.prefix(maxBytes)
            .map { String(format: "%02X", $0) }
            .joined(separator: " ")
    }
}

private enum H264SliceKind {
    case p
    case b
    case i
    case sp
    case si

    var isIntra: Bool {
        self == .i || self == .si
    }
}

private struct H264BitReader {
    let bytes: [UInt8]
    var bitOffset = 0

    mutating func readBit() -> UInt8? {
        guard bitOffset / 8 < bytes.count else { return nil }
        let byte = bytes[bitOffset / 8]
        let shift = 7 - (bitOffset % 8)
        bitOffset += 1
        return (byte >> shift) & 1
    }

    mutating func readBits(_ count: Int) -> UInt32? {
        var value: UInt32 = 0
        for _ in 0..<count {
            guard let bit = readBit() else { return nil }
            value = (value << 1) | UInt32(bit)
        }
        return value
    }

    mutating func readUE() -> UInt32? {
        var leadingZeroBits = 0
        while readBit() == 0 {
            leadingZeroBits += 1
            if leadingZeroBits >= 32 {
                return nil
            }
        }

        let suffix: UInt32
        if leadingZeroBits == 0 {
            suffix = 0
        } else {
            guard let bits = readBits(leadingZeroBits) else { return nil }
            suffix = bits
        }

        return ((UInt32(1) << UInt32(leadingZeroBits)) - 1) + suffix
    }
}

private func findStartCode(
    in bytes: UnsafeBufferPointer<UInt8>,
    count: Int,
    from position: Int
) -> (offset: Int, length: Int)? {
    var index = position
    while index < count - 2 {
        if bytes[index] == 0 && bytes[index + 1] == 0 {
            if bytes[index + 2] == 1 {
                return (index, 3)
            }
            if index + 3 < count && bytes[index + 2] == 0 && bytes[index + 3] == 1 {
                return (index, 4)
            }
        }
        index += 1
    }
    return nil
}

private func readBigEndianLength(from data: Data, offset: Int, width: Int) -> Int {
    var value = 0
    for index in 0..<width {
        value = (value << 8) | Int(data[offset + index])
    }
    return value
}

// MARK: - Helper for parameter set creation

private extension Array where Element == Data {
    /// Runs `body` with a C array of pointers to EVERY element's bytes, all
    /// simultaneously live — what `CMVideoFormatDescriptionCreateFrom*ParameterSets`
    /// needs. Recursion nests one `withUnsafeBytes` scope per element, so this
    /// works for H.264's [SPS, PPS] and HEVC's [VPS, SPS, PPS] alike (an older
    /// version hardcoded two elements and handed VideoToolbox a nil third
    /// pointer, which it rejects as -12712).
    func withUnsafeBufferPointers<R>(
        _ body: (UnsafePointer<UnsafePointer<UInt8>>, UnsafePointer<Int>) -> R
    ) -> R {
        var collected: [UnsafePointer<UInt8>] = []
        collected.reserveCapacity(count)

        func recurse(_ index: Int) -> R {
            if index == count {
                let sizes = map(\.count)
                return collected.withUnsafeBufferPointer { ptrBuf in
                    sizes.withUnsafeBufferPointer { sizeBuf in
                        body(ptrBuf.baseAddress!, sizeBuf.baseAddress!)
                    }
                }
            }
            return self[index].withUnsafeBytes { raw in
                collected.append(raw.bindMemory(to: UInt8.self).baseAddress!)
                return recurse(index + 1)
            }
        }

        return recurse(0)
    }
}

private extension Data {
    func trimmingTrailingZeros() -> Data {
        var trimmed = self
        while trimmed.last == 0 {
            trimmed.removeLast()
        }
        return trimmed
    }
}
