// AVFoundation QR scanner sheet. Presented from ConnectView when
// the user taps "Scan QR". Returns the decoded string via `onScan`; closing the sheet
// without scanning returns nothing.

import AVFoundation
import SwiftUI
import UIKit

struct QRScannerView: UIViewControllerRepresentable {
    let onScan: (String) -> Void
    let onCancel: () -> Void

    func makeUIViewController(context: Context) -> QRScannerViewController {
        let controller = QRScannerViewController()
        controller.onScan = onScan
        controller.onCancel = onCancel
        return controller
    }

    func updateUIViewController(_ uiViewController: QRScannerViewController, context: Context) {}
}

final class QRScannerViewController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var onScan: ((String) -> Void)?
    var onCancel: (() -> Void)?

    private let captureSession = AVCaptureSession()
    private var previewLayer: AVCaptureVideoPreviewLayer?
    private var didFinish = false

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black

        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            configureSession()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                DispatchQueue.main.async {
                    if granted {
                        self?.configureSession()
                    } else {
                        self?.showCameraDeniedAlert()
                    }
                }
            }
        case .denied, .restricted:
            showCameraDeniedAlert()
        @unknown default:
            showCameraDeniedAlert()
        }

        let cancelButton = UIButton(type: .system)
        cancelButton.setTitle("Cancel", for: .normal)
        cancelButton.setTitleColor(.white, for: .normal)
        cancelButton.titleLabel?.font = .systemFont(ofSize: 17, weight: .semibold)
        cancelButton.addTarget(self, action: #selector(handleCancel), for: .touchUpInside)
        cancelButton.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(cancelButton)
        NSLayoutConstraint.activate([
            cancelButton.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 16),
            cancelButton.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -20),
        ])
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        if previewLayer != nil && !captureSession.isRunning {
            DispatchQueue.global(qos: .userInitiated).async { [weak self] in
                self?.captureSession.startRunning()
            }
        }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        if captureSession.isRunning {
            captureSession.stopRunning()
        }
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        previewLayer?.frame = view.bounds
    }

    private func configureSession() {
        guard let device = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: device) else {
            showCameraDeniedAlert()
            return
        }
        if captureSession.canAddInput(input) {
            captureSession.addInput(input)
        }
        let output = AVCaptureMetadataOutput()
        if captureSession.canAddOutput(output) {
            captureSession.addOutput(output)
            output.setMetadataObjectsDelegate(self, queue: DispatchQueue.main)
            output.metadataObjectTypes = [.qr]
        }
        let preview = AVCaptureVideoPreviewLayer(session: captureSession)
        preview.videoGravity = .resizeAspectFill
        preview.frame = view.bounds
        view.layer.insertSublayer(preview, at: 0)
        previewLayer = preview
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            self?.captureSession.startRunning()
        }
    }

    private func showCameraDeniedAlert() {
        let alert = UIAlertController(
            title: "Camera unavailable",
            message: "Enable camera access in Settings to scan QR codes.",
            preferredStyle: .alert
        )
        alert.addAction(UIAlertAction(title: "OK", style: .default) { [weak self] _ in
            self?.handleCancel()
        })
        present(alert, animated: true)
    }

    @objc private func handleCancel() {
        guard !didFinish else { return }
        didFinish = true
        onCancel?()
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        guard !didFinish else { return }
        guard let metadata = metadataObjects.first as? AVMetadataMachineReadableCodeObject,
              metadata.type == .qr,
              let value = metadata.stringValue else { return }
        didFinish = true
        captureSession.stopRunning()
        onScan?(value)
    }
}
