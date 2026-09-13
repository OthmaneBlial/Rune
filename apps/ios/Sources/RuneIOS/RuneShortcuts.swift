import AppIntents
import Foundation

public enum RuneShortcutError: LocalizedError {
    case noDocumentsDirectory

    public var errorDescription: String? {
        switch self {
        case .noDocumentsDirectory:
            return "Rune could not locate its Documents directory."
        }
    }
}

@MainActor
private func executeInDefaultSession(
    _ operation: (RuneFFISession) throws -> RuneCommandResult
) throws -> String {
    guard let root = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first else {
        throw RuneShortcutError.noDocumentsDirectory
    }
    let session = try RuneFFISession(rootURL: root)
    return formatShortcutResult(operation(session))
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

@MainActor
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

@MainActor
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

@MainActor
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

@MainActor
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
            return RuneCommandResult(
                stdout: String(decoding: data, as: UTF8.self),
                stderr: "",
                status: 0
            )
        })
    }
}

public struct RuneShortcuts: AppShortcutsProvider {
    public static var appShortcuts: [AppShortcut] {
        [
            AppShortcut(
                intent: RuneExecuteCommandIntent(),
                phrases: ["Execute a command in \(.applicationName)"],
                shortTitle: "Execute Command",
                systemImageName: "terminal"
            ),
            AppShortcut(
                intent: RuneExecuteScriptIntent(),
                phrases: ["Execute a script in \(.applicationName)"],
                shortTitle: "Execute Script",
                systemImageName: "scroll"
            ),
            AppShortcut(
                intent: RunePutFileIntent(),
                phrases: ["Put a text file in \(.applicationName)"],
                shortTitle: "Put Text File",
                systemImageName: "arrow.down.doc"
            ),
            AppShortcut(
                intent: RuneGetFileIntent(),
                phrases: ["Get a text file from \(.applicationName)"],
                shortTitle: "Get Text File",
                systemImageName: "arrow.up.doc"
            ),
        ]
    }
}
