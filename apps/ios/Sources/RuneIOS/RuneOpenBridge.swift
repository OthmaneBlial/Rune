import Foundation
import RuneFFIHeaders
#if canImport(UIKit)
import UIKit
#endif

/// Source-only Apple adapter for the Rust external-open capability.
///
/// Rust has already validated the URL scheme or confined file path. UIKit
/// performs the actual launch asynchronously on the main queue; returning
/// true means the request was accepted by the adapter, not that the target
/// application has completed opening it.
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
    case 2:
        url = URL(fileURLWithPath: target)
    default:
        return false
    }

#if canImport(UIKit)
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
