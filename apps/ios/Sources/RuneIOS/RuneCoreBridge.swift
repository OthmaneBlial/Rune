import Foundation

private struct RuneFFIOutput {
    let stdout: UnsafeMutablePointer<CChar>?
    let stderr: UnsafeMutablePointer<CChar>?
    let status: Int32
}

private struct RuneFFIFile {
    let data: UnsafeMutablePointer<UInt8>?
    let length: Int
    let status: Int32
    let message: UnsafeMutablePointer<CChar>?
}

@_silgen_name("rune_session_new")
private func rune_session_new(_ root: UnsafePointer<CChar>) -> OpaquePointer?

@_silgen_name("rune_session_new_named")
private func rune_session_new_named(
    _ root: UnsafePointer<CChar>,
    _ sessionID: UnsafePointer<CChar>
) -> OpaquePointer?

@_silgen_name("rune_session_destroy")
private func rune_session_destroy(_ handle: OpaquePointer?)

@_silgen_name("rune_session_cancel")
private func rune_session_cancel(_ handle: OpaquePointer?)

@_silgen_name("rune_session_execute")
private func rune_session_execute(
    _ handle: OpaquePointer?,
    _ input: UnsafePointer<CChar>
) -> RuneFFIOutput

@_silgen_name("rune_session_execute_script")
private func rune_session_execute_script(
    _ handle: OpaquePointer?,
    _ script: UnsafePointer<CChar>
) -> RuneFFIOutput

@_silgen_name("rune_session_put_file")
private func rune_session_put_file(
    _ handle: OpaquePointer?,
    _ path: UnsafePointer<CChar>,
    _ data: UnsafePointer<UInt8>?,
    _ length: Int
) -> RuneFFIOutput

@_silgen_name("rune_session_get_file")
private func rune_session_get_file(
    _ handle: OpaquePointer?,
    _ path: UnsafePointer<CChar>
) -> RuneFFIFile

@_silgen_name("rune_session_current_directory")
private func rune_session_current_directory(_ handle: OpaquePointer?) -> UnsafeMutablePointer<CChar>?

@_silgen_name("rune_session_history")
private func rune_session_history(_ handle: OpaquePointer?) -> UnsafeMutablePointer<CChar>?

@_silgen_name("rune_session_configuration")
private func rune_session_configuration(_ handle: OpaquePointer?) -> UnsafeMutablePointer<CChar>?

@_silgen_name("rune_session_commands")
private func rune_session_commands(_ handle: OpaquePointer?) -> UnsafeMutablePointer<CChar>?

@_silgen_name("rune_session_complete")
private func rune_session_complete(
    _ handle: OpaquePointer?,
    _ input: UnsafePointer<CChar>
) -> UnsafeMutablePointer<CChar>?

@_silgen_name("rune_session_startup_output")
private func rune_session_startup_output(_ handle: OpaquePointer?) -> RuneFFIOutput

@_silgen_name("rune_string_free")
private func rune_string_free(_ value: UnsafeMutablePointer<CChar>?)

@_silgen_name("rune_file_bytes_free")
private func rune_file_bytes_free(_ data: UnsafeMutablePointer<UInt8>?, _ length: Int)

public struct RuneCommandResult: Equatable, Sendable {
    public let stdout: String
    public let stderr: String
    public let status: Int32

    public init(stdout: String, stderr: String, status: Int32) {
        self.stdout = stdout
        self.stderr = stderr
        self.status = status
    }
}

public enum RuneBridgeError: LocalizedError {
    case sessionInitializationFailed(URL)
    case fileOperationFailed(String)

    public var errorDescription: String? {
        switch self {
        case .sessionInitializationFailed(let root):
            return "Rune could not open its sandbox root at \(root.path)."
        case .fileOperationFailed(let message):
            return message
        }
    }
}

public final class RuneFFISession: @unchecked Sendable {
    private var handle: OpaquePointer?
    private let lock = NSLock()

    public init(rootURL: URL, sessionID: String? = nil) throws {
        let created: OpaquePointer?
        if let sessionID {
            created = rootURL.path.withCString { path in
                sessionID.withCString { identifier in
                    rune_session_new_named(path, identifier)
                }
            }
        } else {
            created = rootURL.path.withCString { rune_session_new($0) }
        }
        guard let created else {
            throw RuneBridgeError.sessionInitializationFailed(rootURL)
        }
        handle = created
    }

    deinit {
        rune_session_destroy(handle)
    }

    /// Requests cooperative cancellation for the next Rust execution boundary.
    /// A synchronous operation already in progress may finish first.
    public func cancel() {
        rune_session_cancel(handle)
    }

    public var currentDirectory: String {
        withLock {
            guard let pointer = rune_session_current_directory(handle) else {
                return "~"
            }
            defer { rune_string_free(pointer) }
            return String(cString: pointer)
        }
    }

    public func execute(_ command: String) -> RuneCommandResult {
        withLock {
            let raw = command.withCString { rune_session_execute(handle, $0) }
            return consume(raw)
        }
    }

    public func executeScript(_ script: String) -> RuneCommandResult {
        withLock {
            let raw = script.withCString { rune_session_execute_script(handle, $0) }
            return consume(raw)
        }
    }

    /// Stores bounded bytes in the session's confined virtual filesystem.
    public func putFile(path: String, data: Data) throws {
        let result: RuneCommandResult = withLock {
            let raw = path.withCString { pathPointer in
                data.withUnsafeBytes { (buffer: UnsafeRawBufferPointer) in
                    rune_session_put_file(
                        handle,
                        pathPointer,
                        buffer.bindMemory(to: UInt8.self).baseAddress,
                        buffer.count
                    )
                }
            }
            return consume(raw)
        }
        guard result.status == 0 else {
            throw RuneBridgeError.fileOperationFailed(
                result.stderr.isEmpty ? "Rune could not write the file." : result.stderr
            )
        }
    }

    /// Reads bounded bytes from the session's confined virtual filesystem.
    public func getFile(path: String) throws -> Data {
        try withLock {
            let raw = path.withCString { rune_session_get_file(handle, $0) }
            defer {
                rune_file_bytes_free(raw.data, raw.length)
                rune_string_free(raw.message)
            }
            guard raw.status == 0 else {
                let message = raw.message.map { String(cString: $0) }
                    ?? "Rune could not read the file."
                throw RuneBridgeError.fileOperationFailed(message)
            }
            guard let data = raw.data else {
                return Data()
            }
            return Data(bytes: data, count: raw.length)
        }
    }

    public func takeStartupOutput() -> RuneCommandResult {
        withLock {
            consume(rune_session_startup_output(handle))
        }
    }

    public func history() -> [String] {
        withLock {
            guard let pointer = rune_session_history(handle) else {
                return []
            }
            defer { rune_string_free(pointer) }
            let value = String(cString: pointer)
            guard !value.isEmpty else { return [] }
            return value.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        }
    }

    public var configuration: String {
        withLock {
            guard let pointer = rune_session_configuration(handle) else {
                return ""
            }
            defer { rune_string_free(pointer) }
            return String(cString: pointer)
        }
    }

    public func commands() -> [String] {
        withLock {
            guard let pointer = rune_session_commands(handle) else {
                return []
            }
            defer { rune_string_free(pointer) }
            let value = String(cString: pointer)
            guard !value.isEmpty else { return [] }
            return value.split(separator: "\n").map(String.init)
        }
    }

    public func completionCandidates(for input: String) -> [String] {
        withLock {
            let pointer = input.withCString { rune_session_complete(handle, $0) }
            guard let pointer else {
                return []
            }
            defer { rune_string_free(pointer) }
            let value = String(cString: pointer)
            guard !value.isEmpty else { return [] }
            return value.split(separator: "\n").map(String.init)
        }
    }

    private func consume(_ raw: RuneFFIOutput) -> RuneCommandResult {
        defer {
            rune_string_free(raw.stdout)
            rune_string_free(raw.stderr)
        }
        let stdout = raw.stdout.map { String(cString: $0) } ?? ""
        let stderr = raw.stderr.map { String(cString: $0) } ?? ""
        return RuneCommandResult(stdout: stdout, stderr: stderr, status: raw.status)
    }

    private func withLock<T>(_ operation: () throws -> T) rethrows -> T {
        lock.lock()
        defer { lock.unlock() }
        return try operation()
    }
}
