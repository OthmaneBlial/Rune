import Foundation

private struct RuneFFIOutput {
    let stdout: UnsafeMutablePointer<CChar>?
    let stderr: UnsafeMutablePointer<CChar>?
    let status: Int32
}

private struct RuneFFIEvent {
    let kind: Int32
    let stdout: UnsafePointer<CChar>?
    let stderr: UnsafePointer<CChar>?
    let status: Int32
    let currentDirectory: UnsafePointer<CChar>?
}

private typealias RuneEventCallback = @convention(c) (
    UnsafeRawPointer?,
    UnsafeMutableRawPointer?
) -> Void

private struct RuneFFIFile {
    let data: UnsafeMutablePointer<UInt8>?
    let length: Int
    let status: Int32
    let message: UnsafeMutablePointer<CChar>?
}

private struct RuneClipboardResponse {
    var textLength: Int
    var error: Int32
}

private typealias RuneClipboardReadCallback = @convention(c) (
    UnsafeMutableRawPointer?,
    UnsafeMutablePointer<UInt8>?,
    Int,
    UnsafeMutablePointer<RuneClipboardResponse>?
) -> Bool

private typealias RuneClipboardWriteCallback = @convention(c) (
    UnsafeMutableRawPointer?,
    UnsafePointer<UInt8>?,
    Int
) -> Bool

public enum RuneExecutionEventKind: Int32, Sendable {
    case output = 1
    case status = 2
}

public struct RuneExecutionEvent: Sendable {
    public let kind: RuneExecutionEventKind
    public let stdout: String
    public let stderr: String
    public let status: Int32
    public let currentDirectory: String

    public init(
        kind: RuneExecutionEventKind,
        stdout: String,
        stderr: String,
        status: Int32,
        currentDirectory: String
    ) {
        self.kind = kind
        self.stdout = stdout
        self.stderr = stderr
        self.status = status
        self.currentDirectory = currentDirectory
    }
}

private final class RuneEventCollector {
    var events: [RuneExecutionEvent] = []
}

private let runeEventCallback: RuneEventCallback = { event, userData in
    guard let event, let userData else { return }
    let rawEvent = event.assumingMemoryBound(to: RuneFFIEvent.self).pointee
    let collector = Unmanaged<RuneEventCollector>
        .fromOpaque(userData)
        .takeUnretainedValue()
    guard let kind = RuneExecutionEventKind(rawValue: rawEvent.kind) else {
        return
    }
    collector.events.append(RuneExecutionEvent(
        kind: kind,
        stdout: rawEvent.stdout.map { String(cString: $0) } ?? "",
        stderr: rawEvent.stderr.map { String(cString: $0) } ?? "",
        status: rawEvent.status,
        currentDirectory: rawEvent.currentDirectory.map { String(cString: $0) } ?? ""
    ))
}

@_silgen_name("rune_session_new")
private func rune_session_new(_ root: UnsafePointer<CChar>) -> OpaquePointer?

@_silgen_name("rune_session_new_named")
private func rune_session_new_named(
    _ root: UnsafePointer<CChar>,
    _ sessionID: UnsafePointer<CChar>
) -> OpaquePointer?

@_silgen_name("rune_session_new_with_layout")
private func rune_session_new_with_layout(
    _ home: UnsafePointer<CChar>,
    _ library: UnsafePointer<CChar>,
    _ temporary: UnsafePointer<CChar>
) -> OpaquePointer?

@_silgen_name("rune_session_new_named_with_layout")
private func rune_session_new_named_with_layout(
    _ home: UnsafePointer<CChar>,
    _ library: UnsafePointer<CChar>,
    _ temporary: UnsafePointer<CChar>,
    _ sessionID: UnsafePointer<CChar>
) -> OpaquePointer?

@_silgen_name("rune_session_destroy")
private func rune_session_destroy(_ handle: OpaquePointer?)

@_silgen_name("rune_session_cancel")
private func rune_session_cancel(_ handle: OpaquePointer?)

@_silgen_name("rune_session_set_network_callback")
private func rune_session_set_network_callback(
    _ handle: OpaquePointer?,
    _ callback: RuneNetworkRequestCallback?,
    _ userData: UnsafeMutableRawPointer?
) -> Int32

@_silgen_name("rune_session_set_clipboard_callbacks")
private func rune_session_set_clipboard_callbacks(
    _ handle: OpaquePointer?,
    _ read: RuneClipboardReadCallback?,
    _ write: RuneClipboardWriteCallback?,
    _ userData: UnsafeMutableRawPointer?
) -> Int32

@_silgen_name("rune_session_set_configuration")
private func rune_session_set_configuration(
    _ handle: OpaquePointer?,
    _ key: UnsafePointer<CChar>,
    _ value: UnsafePointer<CChar>
) -> RuneFFIOutput

@_silgen_name("rune_session_reset_configuration")
private func rune_session_reset_configuration(_ handle: OpaquePointer?) -> RuneFFIOutput

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

@_silgen_name("rune_session_execute_with_events")
private func rune_session_execute_with_events(
    _ handle: OpaquePointer?,
    _ input: UnsafePointer<CChar>,
    _ callback: RuneEventCallback?,
    _ userData: UnsafeMutableRawPointer?
) -> RuneFFIOutput

@_silgen_name("rune_session_execute_script_with_events")
private func rune_session_execute_script_with_events(
    _ handle: OpaquePointer?,
    _ script: UnsafePointer<CChar>,
    _ callback: RuneEventCallback?,
    _ userData: UnsafeMutableRawPointer?
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

@_silgen_name("rune_session_history_search")
private func rune_session_history_search(
    _ handle: OpaquePointer?,
    _ query: UnsafePointer<CChar>
) -> UnsafeMutablePointer<CChar>?

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

    public init(
        rootURL: URL,
        libraryURL: URL? = nil,
        temporaryURL: URL? = nil,
        sessionID: String? = nil
    ) throws {
        let created: OpaquePointer?
        if let libraryURL, let temporaryURL {
            created = rootURL.path.withCString { home in
                libraryURL.path.withCString { library in
                    temporaryURL.path.withCString { temporary in
                        if let sessionID {
                            return sessionID.withCString { identifier in
                                rune_session_new_named_with_layout(
                                    home,
                                    library,
                                    temporary,
                                    identifier
                                )
                            }
                        }
                        return rune_session_new_with_layout(home, library, temporary)
                    }
                }
            }
        } else if let sessionID {
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
        _ = rune_session_set_network_callback(created, runeNetworkRequestCallback, nil)
        _ = rune_session_set_clipboard_callbacks(
            created,
            runeClipboardReadCallback,
            runeClipboardWriteCallback,
            nil
        )
    }

    deinit {
        rune_session_destroy(handle)
    }

    /// Requests cooperative cancellation for the next Rust execution boundary.
    /// A synchronous operation already in progress may finish first.
    public func cancel() {
        rune_session_cancel(handle)
    }

    /// Updates one validated Rust-owned setting without adding a shell
    /// command to history. The result includes any persistence failure.
    public func setConfiguration(key: String, value: String) -> RuneCommandResult {
        withLock {
            let raw = key.withCString { keyPointer in
                value.withCString { valuePointer in
                    rune_session_set_configuration(handle, keyPointer, valuePointer)
                }
            }
            return consume(raw)
        }
    }

    /// Restores Rust-owned settings to their defaults without adding a shell
    /// command to history.
    public func resetConfiguration() -> RuneCommandResult {
        withLock {
            consume(rune_session_reset_configuration(handle))
        }
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

    /// Executes a command while collecting bounded Rust events synchronously.
    /// Event strings are copied before the callback returns.
    public func executeWithEvents(_ command: String) -> (RuneCommandResult, [RuneExecutionEvent]) {
        withLock {
            let collector = RuneEventCollector()
            let raw = command.withCString { input in
                rune_session_execute_with_events(
                    handle,
                    input,
                    runeEventCallback,
                    Unmanaged.passUnretained(collector).toOpaque()
                )
            }
            return (consume(raw), collector.events)
        }
    }

    public func executeScript(_ script: String) -> RuneCommandResult {
        withLock {
            let raw = script.withCString { rune_session_execute_script(handle, $0) }
            return consume(raw)
        }
    }

    /// Executes a script while collecting bounded Rust events synchronously.
    public func executeScriptWithEvents(_ script: String) -> (RuneCommandResult, [RuneExecutionEvent]) {
        withLock {
            let collector = RuneEventCollector()
            let raw = script.withCString { input in
                rune_session_execute_script_with_events(
                    handle,
                    input,
                    runeEventCallback,
                    Unmanaged.passUnretained(collector).toOpaque()
                )
            }
            return (consume(raw), collector.events)
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

    /// Returns newest-first bounded history matches without recording a
    /// synthetic search command in the Rust session.
    public func historySearch(_ query: String) -> [String] {
        withLock {
            let pointer = query.withCString { queryPointer in
                rune_session_history_search(handle, queryPointer)
            }
            guard let pointer else {
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
