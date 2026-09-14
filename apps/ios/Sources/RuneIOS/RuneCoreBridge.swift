import Foundation
import RuneFFIHeaders

public enum RuneExecutionEventKind: Int32, Sendable {
    case output = 1
    case status = 2
}

public enum RuneSessionAction: Int32, Equatable, Sendable {
    case none = 0
    case exit = 1
    case newWindow = 2
    case pickFolder = 3
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

private final class RuneEventCollector: @unchecked Sendable {
    var events: [RuneExecutionEvent] = []
    let onEvent: (@Sendable (RuneExecutionEvent) -> Void)?

    init(onEvent: (@Sendable (RuneExecutionEvent) -> Void)? = nil) {
        self.onEvent = onEvent
    }
}

private let runeEventCallback: RuneEventCallback = { event, userData in
    guard let event, let userData else { return }
    let collector = Unmanaged<RuneEventCollector>
        .fromOpaque(userData)
        .takeUnretainedValue()
    guard let kind = RuneExecutionEventKind(rawValue: event.pointee.kind) else {
        return
    }
    let value = RuneExecutionEvent(
        kind: kind,
        stdout: event.pointee.stdout_data.map { String(cString: $0) } ?? "",
        stderr: event.pointee.stderr_data.map { String(cString: $0) } ?? "",
        status: event.pointee.status,
        currentDirectory: event.pointee.current_directory.map { String(cString: $0) } ?? ""
    )
    collector.events.append(value)
    collector.onEvent?(value)
}

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

public struct RuneTerminalStateSnapshot: Decodable, Equatable, Sendable {
    public let columns: Int
    public let rows: Int
    public let cursor: RuneTerminalCursorSnapshot
}

public struct RuneTerminalCursorSnapshot: Decodable, Equatable, Sendable {
    public let row: Int
    public let column: Int
}

/// Versioned metadata from the Rust session. It intentionally contains
/// counts and geometry rather than environment values or terminal text.
public struct RuneSessionSnapshot: Decodable, Equatable, Sendable {
    public let schemaVersion: Int
    public let id: String
    public let workingDirectory: String
    public let historyCount: Int
    public let bookmarkCount: Int
    public let environmentCount: Int
    public let lastStatus: Int32
    public let terminalState: RuneTerminalStateSnapshot

    private enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case id
        case workingDirectory = "working_directory"
        case historyCount = "history_count"
        case bookmarkCount = "bookmark_count"
        case environmentCount = "environment_count"
        case lastStatus = "last_status"
        case terminalState = "terminal_state"
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
    private var handle: UnsafeMutableRawPointer?
    private let lock = NSLock()

    public init(
        rootURL: URL,
        libraryURL: URL? = nil,
        temporaryURL: URL? = nil,
        sessionID: String? = nil
    ) throws {
        let created: UnsafeMutableRawPointer?
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
        _ = rune_session_set_open_callback(created, runeOpenCallback, nil)
    }

    deinit {
        rune_session_destroy(handle)
    }

    /// Requests cooperative cancellation for the next Rust execution boundary.
    /// A synchronous operation already in progress may finish first.
    public func cancel() {
        rune_session_cancel(handle.map(UnsafeRawPointer.init))
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

    /// Clears and persists the Rust-owned terminal screen without adding a
    /// shell command to history.
    public func clearTerminalScreen() -> RuneCommandResult {
        withLock {
            consume(rune_session_clear_terminal(handle))
        }
    }

    /// Resizes the bounded Rust-owned terminal grid for the native viewport.
    /// Rust clamps the requested dimensions to its supported bounds.
    @discardableResult
    public func resizeTerminal(columns: Int, rows: Int) -> Bool {
        guard columns > 0, rows > 0 else { return false }
        return withLock {
            rune_session_resize_terminal(handle, numericCast(columns), numericCast(rows)) == 0
        }
    }

    /// Consumes one host-facing action requested by a Rust command.
    public func takeAction() -> RuneSessionAction {
        withLock {
            RuneSessionAction(rawValue: rune_session_take_action(handle)) ?? .none
        }
    }

    public var currentDirectory: String {
        withLock {
            guard let pointer = rune_session_current_directory(handle.map(UnsafeRawPointer.init)) else {
                return "~"
            }
            defer { rune_string_free(pointer) }
            return String(cString: pointer)
        }
    }

    /// Returns versioned, bounded Rust-owned session metadata. Environment
    /// values and terminal text are intentionally not part of this contract.
    public var sessionSnapshot: RuneSessionSnapshot? {
        withLock {
            guard let pointer = rune_session_snapshot(handle.map(UnsafeRawPointer.init)) else {
                return nil
            }
            defer { rune_string_free(pointer) }
            let data = Data(String(cString: pointer).utf8)
            return try? JSONDecoder().decode(RuneSessionSnapshot.self, from: data)
        }
    }

    /// Returns the current bounded Rust-owned terminal screen. Cursor and
    /// erase controls have already been applied by Rust before this snapshot
    /// crosses the C ABI.
    public var terminalSnapshot: String {
        withLock {
            guard let pointer = rune_session_terminal_snapshot(handle.map(UnsafeRawPointer.init)) else {
                return ""
            }
            defer { rune_string_free(pointer) }
            return String(cString: pointer)
        }
    }

    /// Returns safe, bounded, non-persistent Rust diagnostics for development
    /// and support tooling. Command text, file contents, environment values,
    /// and private paths are not recorded by the core policy.
    public var diagnostics: String {
        withLock {
            guard let pointer = rune_session_diagnostics(handle.map(UnsafeRawPointer.init)) else {
                return ""
            }
            defer { rune_string_free(pointer) }
            return String(cString: pointer)
        }
    }

    /// Clears the in-memory Rust diagnostic buffer without changing session
    /// persistence.
    @discardableResult
    public func clearDiagnostics() -> Bool {
        withLock {
            rune_session_clear_diagnostics(handle) == 0
        }
    }

    /// Returns the current zero-based cursor position for the Rust-owned
    /// terminal screen. Swift can use this to draw a caret without owning a
    /// second cursor state machine.
    public var terminalCursorPosition: (row: Int, column: Int) {
        withLock {
            let cursor = rune_session_terminal_cursor(handle.map(UnsafeRawPointer.init))
            return (row: Int(cursor.row), column: Int(cursor.column))
        }
    }

    /// Returns whether the Rust-owned terminal screen requests a visible caret.
    public var terminalCursorVisible: Bool {
        withLock {
            rune_session_terminal_cursor(handle.map(UnsafeRawPointer.init)).visible
        }
    }

    /// Returns the optional terminal-requested cursor shape override.
    public var terminalCursorShape: String? {
        withLock {
            switch rune_session_terminal_cursor(handle.map(UnsafeRawPointer.init)).shape {
            case 1: return "block"
            case 2: return "underline"
            case 3: return "bar"
            default: return nil
            }
        }
    }

    /// Returns the optional terminal-requested cursor blink override.
    public var terminalCursorBlinking: Bool? {
        withLock {
            switch rune_session_terminal_cursor(handle.map(UnsafeRawPointer.init)).blink {
            case 1: return true
            case 2: return false
            default: return nil
            }
        }
    }

    public func execute(_ command: String) -> RuneCommandResult {
        withLock {
            let raw = command.withCString { rune_session_execute(handle, $0) }
            return consume(raw)
        }
    }

    /// Executes a command while collecting bounded Rust events synchronously.
    /// Event strings are copied before the callback returns; an optional
    /// handler receives each copied event before execution continues.
    public func executeWithEvents(
        _ command: String,
        onEvent: (@Sendable (RuneExecutionEvent) -> Void)? = nil
    ) -> (RuneCommandResult, [RuneExecutionEvent]) {
        withLock {
            let collector = RuneEventCollector(onEvent: onEvent)
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
    /// Event strings are copied before the callback returns; an optional
    /// handler receives each copied event before execution continues.
    public func executeScriptWithEvents(
        _ script: String,
        onEvent: (@Sendable (RuneExecutionEvent) -> Void)? = nil
    ) -> (RuneCommandResult, [RuneExecutionEvent]) {
        withLock {
            let collector = RuneEventCollector(onEvent: onEvent)
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
            let raw = path.withCString {
                rune_session_get_file(handle.map(UnsafeRawPointer.init), $0)
            }
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
            guard let pointer = rune_session_history(handle.map(UnsafeRawPointer.init)) else {
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
                rune_session_history_search(handle.map(UnsafeRawPointer.init), queryPointer)
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
            guard let pointer = rune_session_configuration(handle.map(UnsafeRawPointer.init)) else {
                return ""
            }
            defer { rune_string_free(pointer) }
            return String(cString: pointer)
        }
    }

    public func commands() -> [String] {
        withLock {
            guard let pointer = rune_session_commands(handle.map(UnsafeRawPointer.init)) else {
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
            let pointer = input.withCString {
                rune_session_complete(handle.map(UnsafeRawPointer.init), $0)
            }
            guard let pointer else {
                return []
            }
            defer { rune_string_free(pointer) }
            let value = String(cString: pointer)
            guard !value.isEmpty else { return [] }
            return value.split(separator: "\n").map(String.init)
        }
    }

    /// Applies one current Rust completion candidate and returns the full
    /// replacement command. Token boundaries and suffix rules stay in Rust so
    /// native frontends do not duplicate shell text handling.
    public func completionReplacement(input: String, candidate: String) -> String? {
        withLock {
            let pointer = input.withCString { inputPointer in
                candidate.withCString { candidatePointer in
                    rune_session_apply_completion(
                        handle.map(UnsafeRawPointer.init),
                        inputPointer,
                        candidatePointer
                    )
                }
            }
            guard let pointer else { return nil }
            defer { rune_string_free(pointer) }
            return String(cString: pointer)
        }
    }

    private func consume(_ raw: RuneOutput) -> RuneCommandResult {
        defer {
            rune_string_free(raw.stdout_data)
            rune_string_free(raw.stderr_data)
        }
        let stdout = raw.stdout_data.map { String(cString: $0) } ?? ""
        let stderr = raw.stderr_data.map { String(cString: $0) } ?? ""
        return RuneCommandResult(stdout: stdout, stderr: stderr, status: raw.status)
    }

    private func withLock<T>(_ operation: () throws -> T) rethrows -> T {
        lock.lock()
        defer { lock.unlock() }
        return try operation()
    }
}
