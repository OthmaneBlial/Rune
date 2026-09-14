import Foundation
import RuneFFIHeaders
#if canImport(UIKit)
import UIKit
#endif
#if canImport(AVFoundation)
import AVFoundation
#endif
#if canImport(AVKit)
import AVKit
#endif
#if canImport(QuickLook)
import QuickLook
#endif

#if canImport(UIKit)
private func runePresentingViewController() -> UIViewController? {
    let windows = UIApplication.shared.connectedScenes
        .compactMap { ($0 as? UIWindowScene)?.windows ?? [] }
        .flatMap { $0 }
    guard let root = windows.first(where: { $0.isKeyWindow })?.rootViewController
            ?? windows.first?.rootViewController else {
        return nil
    }
    var current = root
    while let presented = current.presentedViewController {
        current = presented
    }
    return current
}
#endif

#if canImport(UIKit) && canImport(AVFoundation) && canImport(AVKit)
private final class RunePlaybackCoordinator: NSObject {
    static let shared = RunePlaybackCoordinator()

    private var player: AVPlayer?
    private var controller: AVPlayerViewController?

    func present(url: URL) {
        DispatchQueue.main.async { [weak self] in
            guard let self, let presenter = runePresentingViewController() else {
                return
            }
            let player = AVPlayer(url: url)
            let controller = AVPlayerViewController()
            controller.player = player
            controller.allowsPictureInPicturePlayback = true
            self.player = player
            self.controller = controller
            presenter.present(controller, animated: true) { [weak self] in
                self?.player?.play()
            }
        }
    }
}
#endif

#if canImport(UIKit) && canImport(QuickLook)
private final class RunePreviewItem: NSObject, QLPreviewItem {
    let previewItemURL: URL?

    init(url: URL) {
        previewItemURL = url
    }
}

private final class RunePreviewCoordinator: NSObject, QLPreviewControllerDataSource,
    QLPreviewControllerDelegate {
    static let shared = RunePreviewCoordinator()

    private var item: RunePreviewItem?

    func present(url: URL) {
        DispatchQueue.main.async { [weak self] in
            guard let self, let presenter = runePresentingViewController() else {
                return
            }
            self.item = RunePreviewItem(url: url)
            let controller = QLPreviewController()
            controller.dataSource = self
            controller.delegate = self
            presenter.present(controller, animated: true)
        }
    }

    func numberOfPreviewItems(in controller: QLPreviewController) -> Int {
        item == nil ? 0 : 1
    }

    func previewController(
        _ controller: QLPreviewController,
        previewItemAt _: Int
    ) -> QLPreviewItem {
        item ?? RunePreviewItem(url: URL(fileURLWithPath: "/"))
    }

    func previewControllerDidDismiss(_ controller: QLPreviewController) {
        item = nil
    }
}
#endif

/// Source-only Apple adapter for the Rust external interaction capability.
///
/// Rust has already validated the URL scheme or confined file path. UIKit,
/// AVPlayer, or Quick Look performs the actual action asynchronously on the
/// main queue; returning true means the request was accepted by the adapter,
/// not that the target application has completed opening it.
let runeOpenCallback: RuneOpenCallback = { _, targetPointer, targetKind in
    guard let targetPointer else { return false }
    let target = String(cString: targetPointer)
    let url: URL
    switch targetKind {
    case 1:
        guard let parsed = URL(string: target), parsed.scheme != nil else {
            return false
        }
        url = parsed
    case 2, 3, 4:
        url = URL(fileURLWithPath: target)
    default:
        return false
    }

#if canImport(UIKit)
    if targetKind == 3 {
#if canImport(AVFoundation) && canImport(AVKit)
        RunePlaybackCoordinator.shared.present(url: url)
        return true
#else
        return false
#endif
    }
    if targetKind == 4 {
#if canImport(QuickLook)
        RunePreviewCoordinator.shared.present(url: url)
        return true
#else
        return false
#endif
    }
    let open = {
        UIApplication.shared.open(url, options: [:], completionHandler: nil)
    }
    if Thread.isMainThread {
        open()
    } else {
        DispatchQueue.main.async(execute: open)
    }
    return true
#else
    return false
#endif
}
