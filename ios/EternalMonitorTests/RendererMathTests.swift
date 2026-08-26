import XCTest
@testable import EternalMonitor

final class RendererMathTests: XCTestCase {
    func testWideSourceLetterboxes() {
        // 16:9 video on a 4:3 drawable: full width, reduced height.
        let scale = MetalRenderer.aspectFitScale(
            video: CGSize(width: 1920, height: 1080),
            drawable: CGSize(width: 2048, height: 1536)
        )
        XCTAssertEqual(scale.x, 1, accuracy: 0.0001)
        XCTAssertEqual(scale.y, Float((2048.0 / 1536.0) / (1920.0 / 1080.0)), accuracy: 0.0001)
        XCTAssertLessThan(scale.y, 1)
    }

    func testTallSourcePillarboxes() {
        // 9:16 video on a 4:3 drawable: full height, reduced width.
        let scale = MetalRenderer.aspectFitScale(
            video: CGSize(width: 1080, height: 1920),
            drawable: CGSize(width: 2048, height: 1536)
        )
        XCTAssertEqual(scale.y, 1, accuracy: 0.0001)
        XCTAssertLessThan(scale.x, 1)
    }

    func testExactMatchIsUnityScale() {
        let scale = MetalRenderer.aspectFitScale(
            video: CGSize(width: 1280, height: 720),
            drawable: CGSize(width: 2560, height: 1440)
        )
        XCTAssertEqual(scale.x, 1, accuracy: 0.0001)
        XCTAssertEqual(scale.y, 1, accuracy: 0.0001)
    }

    func testDegenerateSizesAreSafe() {
        let scale = MetalRenderer.aspectFitScale(video: .zero, drawable: .zero)
        XCTAssertEqual(scale, SIMD2<Float>(1, 1))
    }
}
