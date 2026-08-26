import SwiftUI
import MetalKit
import CoreVideo
import os

/// SwiftUI wrapper for MTKView that renders decoded video frames via Metal.
///
/// Draw-on-demand: the view is paused and redraws only when a new decoded
/// frame lands in the `FrameSlot` (or the drawable size changes) instead of
/// free-running at 120 Hz re-encoding the same texture.
struct MetalView: UIViewRepresentable {
    @EnvironmentObject var connectionManager: ConnectionManager

    func makeUIView(context: Context) -> MTKView {
        let view = MTKView()
        guard let device = MTLCreateSystemDefaultDevice() else {
            fatalError("Metal is not supported on this device")
        }

        view.device = device
        view.colorPixelFormat = .bgra8Unorm
        view.framebufferOnly = true
        // Redraw only when told to — new frame or size change.
        view.isPaused = true
        view.enableSetNeedsDisplay = true
        view.backgroundColor = UIColor(red: 0.024, green: 0.027, blue: 0.031, alpha: 1) // Theme.void
        view.clearColor = MTLClearColor(red: 0.024, green: 0.027, blue: 0.031, alpha: 1)

        let renderer = MetalRenderer(device: device, view: view)
        context.coordinator.renderer = renderer
        context.coordinator.frameSlot = connectionManager.frameSlot
        context.coordinator.attach(view: view)
        view.delegate = context.coordinator

        return view
    }

    func updateUIView(_ uiView: MTKView, context: Context) {
        // Draw-on-demand makes preferredFramesPerSecond irrelevant: presentation
        // cadence follows decoded frames (naturally capped by the stream fps).
    }

    static func dismantleUIView(_ uiView: MTKView, coordinator: Coordinator) {
        coordinator.detach()
    }

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    final class Coordinator: NSObject, MTKViewDelegate {
        var renderer: MetalRenderer?
        /// Direct reference to the shared FrameSlot — avoids crossing @MainActor boundary on the render thread.
        var frameSlot: FrameSlot?

        private weak var view: MTKView?
        /// Coalesces redraw requests: many decoded frames between two main-loop
        /// turns become one setNeedsDisplay.
        private let redrawPending = OSAllocatedUnfairLock(initialState: false)

        func attach(view: MTKView) {
            self.view = view
            frameSlot?.onFrameStored = { [weak self] in
                self?.scheduleRedraw()
            }
        }

        func detach() {
            frameSlot?.onFrameStored = nil
            view = nil
        }

        /// Called from the VideoToolbox callback thread on every stored frame.
        private func scheduleRedraw() {
            let shouldSchedule = redrawPending.withLock { pending -> Bool in
                if pending { return false }
                pending = true
                return true
            }
            guard shouldSchedule else { return }
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                self.redrawPending.withLock { $0 = false }
                self.view?.setNeedsDisplay()
            }
        }

        func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {
            // Redraw the held frame with the letterbox recomputed for the new size.
            view.setNeedsDisplay()
        }

        func draw(in view: MTKView) {
            guard let renderer else { return }
            let pixelBuffer = frameSlot?.take()
            renderer.draw(in: view, pixelBuffer: pixelBuffer)
        }
    }
}

// MARK: - Metal Renderer

/// Everything one draw needs, bundled so the pixel buffer and its texture
/// wrappers provably outlive GPU sampling: the command buffer's completion
/// handler retains this until the pass finishes. (Retaining only the raw
/// MTLTexture let VideoToolbox recycle the backing IOSurface mid-draw —
/// tearing and green frames.)
private struct FrameTextures {
    let luma: MTLTexture
    let chroma: MTLTexture
    let cvLuma: CVMetalTexture
    let cvChroma: CVMetalTexture
    let pixelBuffer: CVPixelBuffer
    let width: Int
    let height: Int
    /// 0 = BT.709, 1 = BT.601 — matches the shader's DisplayUniforms.
    let matrixIndex: UInt32
}

private struct DisplayUniforms {
    var scale: SIMD2<Float>
    var matrixIndex: UInt32
    // std140-ish padding so the struct matches the Metal-side layout.
    var _padding: UInt32 = 0
}

final class MetalRenderer {
    private let device: MTLDevice
    private let commandQueue: MTLCommandQueue
    private let pipelineState: MTLRenderPipelineState
    private var textureCache: CVMetalTextureCache?
    private var currentFrame: FrameTextures?

    init(device: MTLDevice, view: MTKView) {
        self.device = device

        guard let queue = device.makeCommandQueue() else {
            fatalError("Failed to create Metal command queue")
        }
        self.commandQueue = queue

        // Load shaders
        guard let library = device.makeDefaultLibrary(),
              let vertexFunc = library.makeFunction(name: "fullscreen_vertex"),
              let fragmentFunc = library.makeFunction(name: "display_fragment") else {
            fatalError("Failed to load Metal shaders")
        }

        // Pipeline
        let pipelineDescriptor = MTLRenderPipelineDescriptor()
        pipelineDescriptor.vertexFunction = vertexFunc
        pipelineDescriptor.fragmentFunction = fragmentFunc
        pipelineDescriptor.colorAttachments[0].pixelFormat = view.colorPixelFormat

        do {
            pipelineState = try device.makeRenderPipelineState(descriptor: pipelineDescriptor)
        } catch {
            fatalError("Failed to create pipeline state: \(error)")
        }

        // Texture cache for zero-copy CVPixelBuffer → MTLTexture
        var cache: CVMetalTextureCache?
        CVMetalTextureCacheCreate(kCFAllocatorDefault, nil, device, nil, &cache)
        textureCache = cache
    }

    func draw(in view: MTKView, pixelBuffer: CVPixelBuffer?) {
        if let pixelBuffer, let frame = makeFrameTextures(from: pixelBuffer) {
            currentFrame = frame
            // Let the cache recycle textures released by completed draws.
            if let cache = textureCache {
                CVMetalTextureCacheFlush(cache, 0)
            }
        }

        // If no frame yet, skip the draw (paused view keeps its clear color).
        guard let frame = currentFrame else { return }

        guard let drawable = view.currentDrawable,
              let passDescriptor = view.currentRenderPassDescriptor,
              let commandBuffer = commandQueue.makeCommandBuffer(),
              let encoder = commandBuffer.makeRenderCommandEncoder(descriptor: passDescriptor) else {
            return
        }

        var uniforms = DisplayUniforms(
            scale: Self.aspectFitScale(
                video: CGSize(width: frame.width, height: frame.height),
                drawable: view.drawableSize
            ),
            matrixIndex: frame.matrixIndex
        )

        encoder.setRenderPipelineState(pipelineState)
        encoder.setVertexBytes(&uniforms, length: MemoryLayout<DisplayUniforms>.stride, index: 0)
        encoder.setFragmentBytes(&uniforms, length: MemoryLayout<DisplayUniforms>.stride, index: 0)
        encoder.setFragmentTexture(frame.luma, index: 0)
        encoder.setFragmentTexture(frame.chroma, index: 1)
        encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
        encoder.endEncoding()

        // Keep the pixel buffer + texture wrappers alive until the GPU is done
        // sampling them.
        commandBuffer.addCompletedHandler { _ in
            _ = frame
        }

        commandBuffer.present(drawable)
        commandBuffer.commit()
    }

    /// Scale that fits `video` inside `drawable` preserving aspect ratio
    /// (letterbox/pillarbox). Pure and unit-tested.
    static func aspectFitScale(video: CGSize, drawable: CGSize) -> SIMD2<Float> {
        guard video.width > 0, video.height > 0, drawable.width > 0, drawable.height > 0 else {
            return SIMD2<Float>(1, 1)
        }
        let videoAspect = video.width / video.height
        let drawableAspect = drawable.width / drawable.height
        if videoAspect > drawableAspect {
            // Video is wider: full width, shrink height (letterbox).
            return SIMD2<Float>(1, Float(drawableAspect / videoAspect))
        } else {
            // Video is taller: full height, shrink width (pillarbox).
            return SIMD2<Float>(Float(videoAspect / drawableAspect), 1)
        }
    }

    private func makeFrameTextures(from pixelBuffer: CVPixelBuffer) -> FrameTextures? {
        guard let cache = textureCache else { return nil }
        let width = CVPixelBufferGetWidth(pixelBuffer)
        let height = CVPixelBufferGetHeight(pixelBuffer)
        guard CVPixelBufferGetPlaneCount(pixelBuffer) >= 2 else { return nil }

        var cvLuma: CVMetalTexture?
        guard CVMetalTextureCacheCreateTextureFromImage(
            kCFAllocatorDefault, cache, pixelBuffer, nil,
            .r8Unorm, width, height, 0, &cvLuma
        ) == kCVReturnSuccess, let cvLuma, let luma = CVMetalTextureGetTexture(cvLuma) else {
            return nil
        }

        var cvChroma: CVMetalTexture?
        guard CVMetalTextureCacheCreateTextureFromImage(
            kCFAllocatorDefault, cache, pixelBuffer, nil,
            .rg8Unorm, width / 2, height / 2, 1, &cvChroma
        ) == kCVReturnSuccess, let cvChroma, let chroma = CVMetalTextureGetTexture(cvChroma) else {
            return nil
        }

        let matrixIndex: UInt32
        if let attachment = CVBufferCopyAttachment(pixelBuffer, kCVImageBufferYCbCrMatrixKey, nil),
           CFGetTypeID(attachment) == CFStringGetTypeID(),
           (attachment as! CFString) == kCVImageBufferYCbCrMatrix_ITU_R_601_4 {
            matrixIndex = 1
        } else {
            matrixIndex = 0 // BT.709 default
        }

        return FrameTextures(
            luma: luma,
            chroma: chroma,
            cvLuma: cvLuma,
            cvChroma: cvChroma,
            pixelBuffer: pixelBuffer,
            width: width,
            height: height,
            matrixIndex: matrixIndex
        )
    }
}
