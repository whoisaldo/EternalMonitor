import SwiftUI
import MetalKit
import CoreVideo

/// SwiftUI wrapper for MTKView that renders decoded video frames via Metal.
struct MetalView: UIViewRepresentable {
    @EnvironmentObject var connectionManager: ConnectionManager
    @EnvironmentObject var settings: AppSettings

    func makeUIView(context: Context) -> MTKView {
        let view = MTKView()
        guard let device = MTLCreateSystemDefaultDevice() else {
            fatalError("Metal is not supported on this device")
        }

        view.device = device
        view.colorPixelFormat = .bgra8Unorm
        view.framebufferOnly = true
        view.isPaused = false
        view.enableSetNeedsDisplay = false
        view.preferredFramesPerSecond = 120
        view.backgroundColor = UIColor(red: 0.031, green: 0.031, blue: 0.031, alpha: 1) // #080808

        let renderer = MetalRenderer(device: device, view: view)
        context.coordinator.renderer = renderer
        context.coordinator.connectionManager = connectionManager
        view.delegate = context.coordinator

        return view
    }

    func updateUIView(_ uiView: MTKView, context: Context) {
        if settings.promotionEnabled {
            uiView.preferredFramesPerSecond = UIScreen.main.maximumFramesPerSecond
        } else {
            uiView.preferredFramesPerSecond = settings.targetFPS
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    class Coordinator: NSObject, MTKViewDelegate {
        var renderer: MetalRenderer?
        weak var connectionManager: ConnectionManager?

        func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {}

        func draw(in view: MTKView) {
            guard let renderer, let manager = connectionManager else { return }
            let pixelBuffer = manager.latestFrame
            renderer.draw(in: view, pixelBuffer: pixelBuffer)
        }
    }
}

// MARK: - Metal Renderer

final class MetalRenderer {
    private let device: MTLDevice
    private let commandQueue: MTLCommandQueue
    private let pipelineState: MTLRenderPipelineState
    private var textureCache: CVMetalTextureCache?
    private var lastTexture: MTLTexture?

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
        // Convert pixel buffer to Metal texture (zero-copy)
        if let pixelBuffer, let cache = textureCache {
            let width = CVPixelBufferGetWidth(pixelBuffer)
            let height = CVPixelBufferGetHeight(pixelBuffer)

            var cvTexture: CVMetalTexture?
            let status = CVMetalTextureCacheCreateTextureFromImage(
                kCFAllocatorDefault,
                cache,
                pixelBuffer,
                nil,
                .bgra8Unorm,
                width,
                height,
                0,
                &cvTexture
            )

            if status == kCVReturnSuccess, let cvTexture,
               let texture = CVMetalTextureGetTexture(cvTexture) {
                lastTexture = texture
            }
        }

        // If no texture yet, skip draw (show black)
        guard let texture = lastTexture else { return }

        guard let drawable = view.currentDrawable,
              let passDescriptor = view.currentRenderPassDescriptor,
              let commandBuffer = commandQueue.makeCommandBuffer(),
              let encoder = commandBuffer.makeRenderCommandEncoder(descriptor: passDescriptor) else {
            return
        }

        encoder.setRenderPipelineState(pipelineState)
        encoder.setFragmentTexture(texture, index: 0)
        encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
        encoder.endEncoding()

        commandBuffer.present(drawable)
        commandBuffer.commit()
    }
}
