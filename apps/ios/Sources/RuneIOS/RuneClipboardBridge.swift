import Foundation
#if canImport(UIKit)
import UIKit
#endif

private let runeClipboardMaximumBytes = 1024 * 1024

struct RuneClipboardResponse {
    var textLength: Int = 0
    var error: Int32 = 0
}

typealias RuneClipboardReadCallback = @convention(c) (
    UnsafeMutableRawPointer?,
    UnsafeMutablePointer<UInt8>?,
    Int,
    UnsafeMutablePointer<RuneClipboardResponse>?
) -> Bool

typealias RuneClipboardWriteCallback = @convention(c) (
    UnsafeMutableRawPointer?,
    UnsafePointer<UInt8>?,
    Int
) -> Bool

let runeClipboardReadCallback: RuneClipboardReadCallback = {
    _, buffer, capacity, response in
    guard let response, capacity >= 0 else { return false }
    response.pointee = RuneClipboardResponse()
#if canImport(UIKit)
    guard let text = UIPasteboard.general.string else {
        response.pointee.error = 1
        return false
    }
    let data = Data(text.utf8)
    guard data.count <= runeClipboardMaximumBytes,
          data.count <= capacity,
          data.isEmpty || buffer != nil else {
        response.pointee.error = 2
        return false
    }
    if !data.isEmpty, let buffer {
        data.withUnsafeBytes { rawBuffer in
            if let source = rawBuffer.bindMemory(to: UInt8.self).baseAddress {
                buffer.update(from: source, count: data.count)
            }
        }
    }
    response.pointee.textLength = data.count
    return true
#else
    response.pointee.error = 1
    return false
#endif
}

let runeClipboardWriteCallback: RuneClipboardWriteCallback = {
    _, text, length in
    guard length >= 0,
          length <= runeClipboardMaximumBytes,
          length == 0 || text != nil else { return false }
#if canImport(UIKit)
    let value: String
    if length == 0 {
        value = ""
    } else if let text {
        let data = Data(bytes: text, count: length)
        guard let decoded = String(data: data, encoding: .utf8) else { return false }
        value = decoded
    } else {
        return false
    }
    UIPasteboard.general.string = value
    return true
#else
    return false
#endif
}
