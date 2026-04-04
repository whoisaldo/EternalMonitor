import Foundation
import VideoToolbox
import CoreMedia

/// H.264 hardware decoder using VideoToolbox.
/// Parses NAL units, extracts SPS/PPS, converts Annex B → AVCC, decodes to CVPixelBuffer.
final class VideoDecoder {
    var onFrameDecoded: ((_ pixelBuffer: CVPixelBuffer, _ timestampUs: UInt64) -> Void)?
    var onEvent: ((String) -> Void)?

    private var formatDescription: CMVideoFormatDescription?
    private var decompressionSession: VTDecompressionSession?
    private var sps: Data?
    private var pps: Data?
    private var callbackRecord: UnsafeMutablePointer<VTDecompressionOutputCallbackRecord>?
    private var loggedPacketizations = Set<String>()
    private var loggedNALTypes = Set<UInt8>()
    private var hasLoggedEmptyPayload = false

    private let decodeQueue = DispatchQueue(label: "com.eternal.decode", qos: .userInteractive)

    deinit {
        callbackRecord?.deallocate()
    }

    func decode(packet: FramePacket) {
        decodeQueue.async { [weak self] in
            self?.decodeOnQueue(packet: packet)
        }
    }

    func invalidate() {
        decodeQueue.async { [weak self] in
            if let session = self?.decompressionSession {
                VTDecompressionSessionWaitForAsynchronousFrames(session)
                VTDecompressionSessionInvalidate(session)
            }
            self?.decompressionSession = nil
            self?.formatDescription = nil
            self?.sps = nil
            self?.pps = nil
        }
    }

    // MARK: - Internal

    private func decodeOnQueue(packet: FramePacket) {
        let parsed = parseNALUnits(from: packet.data)
        if loggedPacketizations.insert(parsed.packetization).inserted {
            onEvent?("Detected \(parsed.packetization) H.264 packetization")
        }

        let nalUnits = parsed.units
        guard !nalUnits.isEmpty else {
            if !hasLoggedEmptyPayload {
                onEvent?("No H.264 NAL units parsed from packet bytes=\(packet.data.count)")
                hasLoggedEmptyPayload = true
            }
            return
        }
        hasLoggedEmptyPayload = false

        for nal in nalUnits {
            let nalType = nal[0] & 0x1F
            logNALTypeIfNeeded(nalType)

            switch nalType {
            case 7: // SPS
                sps = nal
                tryCreateFormatDescription()
            case 8: // PPS
                pps = nal
                tryCreateFormatDescription()
            case 1, 5: // Non-IDR slice, IDR slice
                decodeSlice(nal, timestampUs: packet.timestampUs)
            default:
                break
            }
        }
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

        if let existing = formatDescription,
           CMFormatDescriptionEqual(existing, otherFormatDescription: newFormat) {
            return
        }

        formatDescription = newFormat
        onEvent?("Updated format description SPS=\(sps.count)B PPS=\(pps.count)B")
        createDecompressionSession()
    }

    // MARK: - Decompression session

    private func createDecompressionSession() {
        if let session = decompressionSession {
            VTDecompressionSessionInvalidate(session)
            decompressionSession = nil
        }

        // Clean up old callback record
        callbackRecord?.deallocate()
        callbackRecord = nil

        guard let formatDescription else { return }

        let outputAttributes: [String: Any] = [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
            kCVPixelBufferMetalCompatibilityKey as String: true,
        ]

        // Allocate callback record on heap so it outlives this function
        let record = UnsafeMutablePointer<VTDecompressionOutputCallbackRecord>.allocate(capacity: 1)
        record.initialize(to: VTDecompressionOutputCallbackRecord(
            decompressionOutputCallback: decompressionCallback,
            decompressionOutputRefCon: Unmanaged.passUnretained(self).toOpaque()
        ))
        callbackRecord = record

        var session: VTDecompressionSession?
        let status = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            formatDescription: formatDescription,
            decoderSpecification: [
                kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder: true
            ] as CFDictionary,
            imageBufferAttributes: outputAttributes as CFDictionary,
            outputCallback: record,
            decompressionSessionOut: &session
        )

        guard status == noErr, let session else {
            print("[VideoDecoder] Failed to create decompression session: \(status)")
            onEvent?("Failed to create VideoToolbox session status=\(status)")
            return
        }

        VTSessionSetProperty(session, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        decompressionSession = session
        onEvent?("VideoToolbox session ready")
    }

    // MARK: - Decode a slice NAL unit

    private func decodeSlice(_ nalData: Data, timestampUs: UInt64) {
        guard let session = decompressionSession, let formatDescription else {
            onEvent?("Dropped slice bytes=\(nalData.count) timestampUs=\(timestampUs) because decoder is not ready")
            return
        }

        // Convert Annex B NAL to AVCC: prepend 4-byte big-endian length
        var nalLength = UInt32(nalData.count).bigEndian
        var avccData = Data(bytes: &nalLength, count: 4)
        avccData.append(nalData)

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
            onEvent?("Failed to allocate block buffer for slice bytes=\(nalData.count)")
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
            onEvent?("Failed to create sample buffer for slice bytes=\(nalData.count)")
            return
        }

        // Pass timestampUs through frameReferenceContext so the callback can retrieve it
        let frameRef = UnsafeMutableRawPointer(bitPattern: UInt(truncatingIfNeeded: timestampUs))

        var infoFlags = VTDecodeInfoFlags()
        let status = VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: sampleBuffer,
            flags: [._EnableAsynchronousDecompression],
            frameRefcon: frameRef,
            infoFlagsOut: &infoFlags
        )
        if status != noErr {
            onEvent?("VTDecodeFrame failed status=\(status) bytes=\(nalData.count)")
        }
    }
}

// MARK: - VTDecompressionSession callback

private func decompressionCallback(
    decompressionOutputRefCon: UnsafeMutableRawPointer?,
    sourceFrameRefCon: UnsafeMutableRawPointer?,
    status: OSStatus,
    infoFlags: VTDecodeInfoFlags,
    imageBuffer: CVImageBuffer?,
    presentationTimeStamp: CMTime,
    presentationDuration: CMTime
) {
    guard let refCon = decompressionOutputRefCon else { return }

    let decoder = Unmanaged<VideoDecoder>.fromOpaque(refCon).takeUnretainedValue()
    guard status == noErr, let pixelBuffer = imageBuffer else {
        decoder.onEvent?("Decoder callback status=\(status) imageBufferMissing=\(imageBuffer == nil)")
        return
    }

    let timestampUs = UInt64(UInt(bitPattern: sourceFrameRefCon))
    decoder.onFrameDecoded?(pixelBuffer, timestampUs)
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
    func withUnsafeBufferPointers<R>(
        _ body: (UnsafePointer<UnsafePointer<UInt8>>, UnsafePointer<Int>) -> R
    ) -> R {
        var pointers = [UnsafePointer<UInt8>?](repeating: nil, count: count)
        var sizes = [Int](repeating: 0, count: count)

        for (i, data) in self.enumerated() {
            sizes[i] = data.count
        }

        return self[0].withUnsafeBytes { spsPtr in
            self[1].withUnsafeBytes { ppsPtr in
                pointers[0] = spsPtr.bindMemory(to: UInt8.self).baseAddress
                pointers[1] = ppsPtr.bindMemory(to: UInt8.self).baseAddress
                return pointers.withUnsafeBufferPointer { ptrBuf in
                    sizes.withUnsafeBufferPointer { sizeBuf in
                        body(
                            UnsafeRawPointer(ptrBuf.baseAddress!).assumingMemoryBound(to: UnsafePointer<UInt8>.self),
                            sizeBuf.baseAddress!
                        )
                    }
                }
            }
        }
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
