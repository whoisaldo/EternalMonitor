import Foundation
import VideoToolbox
import CoreMedia

/// H.264 hardware decoder using VideoToolbox.
/// Parses NAL units, extracts SPS/PPS, converts Annex B → AVCC, decodes to CVPixelBuffer.
final class VideoDecoder {
    var onFrameDecoded: ((_ pixelBuffer: CVPixelBuffer, _ timestampUs: UInt64) -> Void)?

    private var formatDescription: CMVideoFormatDescription?
    private var decompressionSession: VTDecompressionSession?
    private var sps: Data?
    private var pps: Data?
    private var callbackRecord: UnsafeMutablePointer<VTDecompressionOutputCallbackRecord>?

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
        let nalUnits = parseNALUnits(from: packet.data)

        for nal in nalUnits {
            let nalType = nal[0] & 0x1F

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

    private func parseNALUnits(from data: Data) -> [Data] {
        var units: [Data] = []
        var i = 0
        let count = data.count

        data.withUnsafeBytes { raw in
            let bytes = raw.bindMemory(to: UInt8.self)

            func findStartCode(from pos: Int) -> (offset: Int, length: Int)? {
                var j = pos
                while j < count - 2 {
                    if bytes[j] == 0 && bytes[j + 1] == 0 {
                        if bytes[j + 2] == 1 {
                            return (j, 3)
                        }
                        if j + 3 < count && bytes[j + 2] == 0 && bytes[j + 3] == 1 {
                            return (j, 4)
                        }
                    }
                    j += 1
                }
                return nil
            }

            guard let first = findStartCode(from: 0) else { return }
            i = first.offset + first.length

            while i < count {
                if let next = findStartCode(from: i) {
                    let nalData = Data(bytes: raw.baseAddress! + i, count: next.offset - i)
                    if !nalData.isEmpty { units.append(nalData) }
                    i = next.offset + next.length
                } else {
                    let nalData = Data(bytes: raw.baseAddress! + i, count: count - i)
                    if !nalData.isEmpty { units.append(nalData) }
                    break
                }
            }
        }

        return units
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
            return
        }

        if let existing = formatDescription,
           CMFormatDescriptionEqual(existing, otherFormatDescription: newFormat) {
            return
        }

        formatDescription = newFormat
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
            return
        }

        VTSessionSetProperty(session, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        decompressionSession = session
    }

    // MARK: - Decode a slice NAL unit

    private func decodeSlice(_ nalData: Data, timestampUs: UInt64) {
        guard let session = decompressionSession, let formatDescription else { return }

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

        guard let blockBuffer else { return }

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

        guard let sampleBuffer else { return }

        // Pass timestampUs through frameReferenceContext so the callback can retrieve it
        let frameRef = UnsafeMutableRawPointer(bitPattern: UInt(truncatingIfNeeded: timestampUs))

        var infoFlags = VTDecodeInfoFlags()
        VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: sampleBuffer,
            flags: [._EnableAsynchronousDecompression],
            frameRefcon: frameRef,
            infoFlagsOut: &infoFlags
        )
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
    guard status == noErr,
          let refCon = decompressionOutputRefCon,
          let pixelBuffer = imageBuffer else { return }

    let decoder = Unmanaged<VideoDecoder>.fromOpaque(refCon).takeUnretainedValue()
    let timestampUs = UInt64(UInt(bitPattern: sourceFrameRefCon))
    decoder.onFrameDecoded?(pixelBuffer, timestampUs)
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
