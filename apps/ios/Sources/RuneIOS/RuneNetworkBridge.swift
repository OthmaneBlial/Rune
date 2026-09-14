import Foundation
import RuneFFIHeaders

private let runeNetworkMaximumBodyBytes = 8 * 1024 * 1024

private final class RuneURLSessionDelegate: NSObject, URLSessionDataDelegate {
    private let completion: DispatchSemaphore
    private let maximumBytes: Int

    private(set) var body = Data()
    private(set) var statusCode: Int32?
    private(set) var transportError: Error?
    private(set) var exceededLimit = false

    init(completion: DispatchSemaphore, maximumBytes: Int) {
        self.completion = completion
        self.maximumBytes = maximumBytes
    }

    func urlSession(
        _ session: URLSession,
        dataTask: URLSessionDataTask,
        didReceive response: URLResponse,
        completionHandler: @escaping (URLSession.ResponseDisposition) -> Void
    ) {
        guard let response = response as? HTTPURLResponse else {
            transportError = NSError(
                domain: "RuneNetwork",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey: "host returned a non-HTTP response"]
            )
            completionHandler(.cancel)
            return
        }
        statusCode = Int32(response.statusCode)
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        guard !exceededLimit else { return }
        guard data.count <= maximumBytes - body.count else {
            exceededLimit = true
            dataTask.cancel()
            return
        }
        body.append(data)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        if transportError == nil {
            transportError = error
        }
        completion.signal()
    }
}

let runeNetworkRequestCallback: RuneNetworkRequestCallback = {
    _, methodPointer, urlPointer, headersPointer, bodyPointer, bodyLength,
    responseBuffer, responseCapacity, responsePointer in
    guard let responsePointer else { return false }
    responsePointer.pointee = RuneNetworkResponse()

    guard let methodPointer, let urlPointer,
          let url = URL(string: String(cString: urlPointer)) else {
        responsePointer.pointee.error = 3
        return false
    }
    guard bodyLength >= 0, bodyLength <= runeNetworkMaximumBodyBytes,
          responseCapacity >= runeNetworkMaximumBodyBytes,
          bodyLength == 0 || bodyPointer != nil else {
        responsePointer.pointee.error = 3
        return false
    }

    var request = URLRequest(url: url)
    request.httpMethod = String(cString: methodPointer)
    request.timeoutInterval = 60
    if bodyLength > 0, let bodyPointer {
        request.httpBody = Data(bytes: bodyPointer, count: bodyLength)
    }
    if let headersPointer {
        let headerText = String(cString: headersPointer)
        for line in headerText.split(separator: "\n", omittingEmptySubsequences: true) {
            guard let separator = line.firstIndex(of: ":") else { continue }
            let name = String(line[..<separator]).trimmingCharacters(in: .whitespaces)
            let value = String(line[line.index(after: separator)...])
                .trimmingCharacters(in: .whitespaces)
            request.setValue(value, forHTTPHeaderField: name)
        }
    }

    let completion = DispatchSemaphore(value: 0)
    let delegate = RuneURLSessionDelegate(
        completion: completion,
        maximumBytes: runeNetworkMaximumBodyBytes
    )
    let session = URLSession(configuration: .ephemeral, delegate: delegate, delegateQueue: nil)
    let task = session.dataTask(with: request)
    task.resume()
    guard completion.wait(timeout: .now() + 65) == .success else {
        task.cancel()
        session.invalidateAndCancel()
        responsePointer.pointee.error = 2
        return false
    }
    session.invalidateAndCancel()

    guard !delegate.exceededLimit, delegate.transportError == nil,
          let statusCode = delegate.statusCode else {
        responsePointer.pointee.error = delegate.exceededLimit ? 1 : 2
        return false
    }
    guard delegate.body.count <= responseCapacity,
          delegate.body.isEmpty || responseBuffer != nil else {
        responsePointer.pointee.error = 1
        return false
    }
    if !delegate.body.isEmpty, let responseBuffer {
        delegate.body.withUnsafeBytes { rawBuffer in
            if let source = rawBuffer.bindMemory(to: UInt8.self).baseAddress {
                responseBuffer.update(from: source, count: delegate.body.count)
            }
        }
    }
    responsePointer.pointee.status_code = statusCode
    responsePointer.pointee.body_length = delegate.body.count
    return true
}
