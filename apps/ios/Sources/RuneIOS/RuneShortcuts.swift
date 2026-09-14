import AppIntents
import Foundation

public enum RuneShortcutError: LocalizedError {
    case noDocumentsDirectory
    case invalidSessionIdentifier
    case invalidUTF8File(String)

    public var errorDescription: String? {
        switch self {
        case .noDocumentsDirectory:
            return "Rune could not locate its Documents directory."
        case .invalidSessionIdentifier:
            return "Rune session identifiers must be 1–64 ASCII letters, digits, '.', '-' or '_'."
        case .invalidUTF8File(let path):
            return "Rune file is not valid UTF-8: \(path)."
        }
    }
}

private func executeInSession(
    _ sessionID: String?,
    _ operation: (RuneFFISession) throws -> RuneCommandResult
) throws -> String {
    guard let root = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first else {
        throw RuneShortcutError.noDocumentsDirectory
    }
    if let sessionID, !isValidSessionIdentifier(sessionID) {
        throw RuneShortcutError.invalidSessionIdentifier
    }
    let library = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask).first
    let temporary = FileManager.default.temporaryDirectory
    let session = try RuneFFISession(
        rootURL: root,
        libraryURL: library,
        temporaryURL: temporary,
        sessionID: sessionID
    )
    return formatShortcutResult(try operation(session))
}

private func executeInDefaultSession(
    _ operation: (RuneFFISession) throws -> RuneCommandResult
) throws -> String {
    try executeInSession(nil, operation)
}

private func isValidSessionIdentifier(_ value: String) -> Bool {
    !value.isEmpty
        && value.count <= 64
        && value.utf8.allSatisfy {
            ($0 >= 65 && $0 <= 90)
                || ($0 >= 97 && $0 <= 122)
                || ($0 >= 48 && $0 <= 57)
                || $0 == 95
                || $0 == 45
                || $0 == 46
        }
}

private func formatShortcutResult(_ result: RuneCommandResult) -> String {
    var text = result.stdout
    if !result.stderr.isEmpty {
        if !text.isEmpty, !text.hasSuffix("\n") {
            text.append("\n")
        }
        text.append(result.stderr)
    }
    if result.status != 0 {
        text.append("[exit \(result.status)]\n")
    }
    return text
}

public struct RuneExecuteCommandIntent: AppIntent {
    public static let title: LocalizedStringResource = "Execute Rune Command"
    public static let description = IntentDescription("Run one command through Rune's Rust session.")

    @Parameter(title: "Command")
    public var command: String

    public init() {
        command = ""
    }

    public func perform() async throws -> some IntentResult & ReturnsValue<String> {
        .result(value: try executeInDefaultSession { session in
            session.execute(command)
        })
    }
}

public struct RuneExecuteScriptIntent: AppIntent {
    public static let title: LocalizedStringResource = "Execute Rune Script"
    public static let description = IntentDescription("Run a bounded newline-delimited script through Rune's Rust session.")

    @Parameter(title: "Script")
    public var script: String

    public init() {
        script = ""
    }

    public func perform() async throws -> some IntentResult & ReturnsValue<String> {
        .result(value: try executeInDefaultSession { session in
            session.executeScript(script)
        })
    }
}

public struct RuneExecuteScriptInSessionIntent: AppIntent {
    public static let title: LocalizedStringResource = "Execute Rune Script in Session"
    public static let description = IntentDescription(
        "Run a bounded script through a named, persisted Rune Rust session."
    )

    @Parameter(title: "Session ID")
    public var sessionID: String

    @Parameter(title: "Script")
    public var script: String

    public init() {
        sessionID = "default"
        script = ""
    }

    public func perform() async throws -> some IntentResult & ReturnsValue<String> {
        .result(value: try executeInSession(sessionID) { session in
            session.executeScript(script)
        })
    }
}

public struct RuneExecuteCommandInSessionIntent: AppIntent {
    public static let title: LocalizedStringResource = "Execute Rune Command in Session"
    public static let description = IntentDescription(
        "Run one command through a named, persisted Rune Rust session."
    )

    @Parameter(title: "Session ID")
    public var sessionID: String

    @Parameter(title: "Command")
    public var command: String

    public init() {
        sessionID = "default"
        command = ""
    }

    public func perform() async throws -> some IntentResult & ReturnsValue<String> {
        .result(value: try executeInSession(sessionID) { session in
            session.execute(command)
        })
    }
}

public struct RunePutFileIntent: AppIntent {
    public static let title: LocalizedStringResource = "Put Text File in Rune"
    public static let description = IntentDescription(
        "Write UTF-8 text to a bounded path in Rune's sandbox."
    )

    @Parameter(title: "Path")
    public var path: String

    @Parameter(title: "Contents")
    public var contents: String

    public init() {
        path = ""
        contents = ""
    }

    public func perform() async throws -> some IntentResult & ReturnsValue<String> {
        .result(value: try executeInDefaultSession { session in
            try session.putFile(path: path, data: Data(contents.utf8))
            return RuneCommandResult(stdout: "Stored \(path)\n", stderr: "", status: 0)
        })
    }
}

public struct RuneGetFileIntent: AppIntent {
    public static let title: LocalizedStringResource = "Get Text File from Rune"
    public static let description = IntentDescription(
        "Read a bounded UTF-8 text file from Rune's sandbox."
    )

    @Parameter(title: "Path")
    public var path: String

    public init() {
        path = ""
    }

    public func perform() async throws -> some IntentResult & ReturnsValue<String> {
        .result(value: try executeInDefaultSession { session in
            let data = try session.getFile(path: path)
            guard let text = String(data: data, encoding: .utf8) else {
                throw RuneShortcutError.invalidUTF8File(path)
            }
            return RuneCommandResult(
                stdout: text,
                stderr: "",
                status: 0
            )
        })
    }
}

public struct RuneShortcuts: AppShortcutsProvider {
    public static var appShortcuts: [AppShortcut] {
            AppShortcut(
                intent: RuneExecuteCommandIntent(),
                phrases: ["Execute a command in \(.applicationName)"],
                shortTitle: "Execute Command",
                systemImageName: "terminal"
            )
            AppShortcut(
                intent: RuneExecuteScriptIntent(),
                phrases: ["Execute a script in \(.applicationName)"],
                shortTitle: "Execute Script",
                systemImageName: "scroll"
            )
            AppShortcut(
                intent: RuneExecuteScriptInSessionIntent(),
                phrases: ["Execute a script in a Rune session in \(.applicationName)"],
                shortTitle: "Execute Script in Session",
                systemImageName: "rectangle.connected.to.line.below"
            )
            AppShortcut(
                intent: RuneExecuteCommandInSessionIntent(),
                phrases: ["Execute a command in a Rune session in \(.applicationName)"],
                shortTitle: "Execute in Session",
                systemImageName: "rectangle.connected.to.line.below"
            )
            AppShortcut(
                intent: RunePutFileIntent(),
                phrases: ["Put a text file in \(.applicationName)"],
                shortTitle: "Put Text File",
                systemImageName: "arrow.down.doc"
            )
            AppShortcut(
                intent: RuneGetFileIntent(),
                phrases: ["Get a text file from \(.applicationName)"],
                shortTitle: "Get Text File",
                systemImageName: "arrow.up.doc"
            )
    }
}
