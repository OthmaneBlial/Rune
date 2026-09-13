import SwiftUI
import Combine
import UniformTypeIdentifiers

public struct RuneTranscriptEntry: Identifiable, Hashable, Sendable {
    public enum Kind: Hashable, Sendable {
        case command
        case stdout
        case stderr
        case status
    }

    public let id = UUID()
    public let kind: Kind
    public let text: String

    public init(kind: Kind, text: String) {
        self.kind = kind
        self.text = text
    }
}

@MainActor
public final class RuneTerminalModel: ObservableObject {
    private static let clearSequence = "\u{1b}[2J\u{1b}[H"
    private static let clearScreenControl = "\u{1b}[2J"
    private static let cursorHomeControl = "\u{1b}[H"
    private static let maximumTranscriptEntries = 4_096
    private static let maximumTranscriptBytes = 8 * 1024 * 1024

    @Published public private(set) var entries: [RuneTranscriptEntry] = []
    @Published public var command = ""
    @Published public private(set) var currentDirectory = "~"
    @Published public private(set) var workspaceName = "Documents"
    @Published public private(set) var fontSize: CGFloat = 15
    @Published public private(set) var theme = "ink"
    @Published public private(set) var initializationError: String?
    @Published public private(set) var isExecuting = false

    private var session: RuneFFISession?
    private var scopedFolder: RuneScopedFolder?
    private let sessionID: String?
    private var history: [String] = []
    private var historyCursor: Int?
    private var executionTask: Task<Void, Never>?
    private var transcriptBytes = 0

    public init(rootURL: URL? = nil, sessionID: String? = nil) {
        self.sessionID = sessionID
        let root = rootURL ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
        let folderAccess = RuneExternalFolderAccess.shared
        session = nil
        scopedFolder = nil
        guard let root else {
            initializationError = "Rune could not locate the app Documents directory."
            return
        }
        if rootURL == nil, sessionID == nil, let name = folderAccess.lastActiveName,
           let scope = try? folderAccess.open(named: name) {
            do {
                let restoredSession = try RuneFFISession(rootURL: scope.url)
                session = restoredSession
                scopedFolder = scope
                workspaceName = scope.url.lastPathComponent.isEmpty ? "Folder" : scope.url.lastPathComponent
                currentDirectory = restoredSession.currentDirectory
                history = restoredSession.history()
                append(restoredSession.takeStartupOutput())
                refreshConfiguration()
                return
            } catch {
                scope.stopAccessing()
                initializationError = error.localizedDescription
            }
        }
        do {
            session = try RuneFFISession(rootURL: root, sessionID: sessionID)
            initializationError = nil
            workspaceName = root.lastPathComponent.isEmpty ? "Documents" : root.lastPathComponent
            currentDirectory = session?.currentDirectory ?? "~"
            history = session?.history() ?? []
            if let startup = session?.takeStartupOutput() {
                if !startup.stdout.isEmpty {
                    appendEntry(.init(kind: .stdout, text: startup.stdout))
                }
                if !startup.stderr.isEmpty {
                    appendEntry(.init(kind: .stderr, text: startup.stderr))
                }
                if startup.status != 0 {
                    appendEntry(.init(kind: .status, text: "[profile exit \(startup.status)]"))
                }
            }
            refreshConfiguration()
        } catch {
            session = nil
            initializationError = error.localizedDescription
        }
    }

    /// Opens a user-selected directory as a new Rust session root and stores
    /// an Apple security-scoped bookmark for the next launch.
    public func openFolder(_ url: URL) {
        guard !isExecuting else {
            initializationError = "Rune is still executing a command. Cancel it before opening a folder."
            return
        }
        do {
            let access = RuneExternalFolderAccess.shared
            let name = access.suggestedName(for: url)
            try access.save(url: url, as: name)
            let scope = try access.open(url: url)
            let nextSession = try RuneFFISession(rootURL: scope.url, sessionID: sessionID)
            let previousScope = scopedFolder
            session = nextSession
            scopedFolder = scope
            workspaceName = scope.url.lastPathComponent.isEmpty ? "Folder" : scope.url.lastPathComponent
            currentDirectory = nextSession.currentDirectory
            history = nextSession.history()
            historyCursor = nil
            command = ""
            clearTranscript()
            initializationError = nil
            append(nextSession.takeStartupOutput())
            refreshConfiguration()
            previousScope?.stopAccessing()
        } catch {
            initializationError = error.localizedDescription
        }
    }

    public func reportFolderImportError(_ error: Error) {
        initializationError = error.localizedDescription
    }

    public func submit() {
        guard !isExecuting else { return }
        let line = command.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !line.isEmpty else { return }
        appendEntry(.init(kind: .command, text: "\(currentDirectory) $ \(line)"))
        command = ""
        historyCursor = nil
        guard let session else {
            appendEntry(.init(kind: .stderr, text: initializationError ?? "Rune session unavailable."))
            return
        }
        isExecuting = true
        executionTask = Task { [weak self, session] in
            let execution = await Task.detached(priority: .userInitiated) {
                session.executeWithEvents(line)
            }.value
            self?.finishExecution(execution.0, events: execution.1, session: session)
        }
    }

    public func cancel() {
        guard isExecuting else { return }
        session?.cancel()
    }

    private func finishExecution(
        _ result: RuneCommandResult,
        events: [RuneExecutionEvent],
        session: RuneFFISession
    ) {
        if events.isEmpty {
            append(result)
        } else {
            events.forEach(append)
            let reportedStatus = events.reversed().compactMap { event in
                event.kind == .status ? event.status : nil
            }.first
            if reportedStatus != result.status {
                appendOutput(stdout: "", stderr: result.stderr)
                if result.status != 0 {
                    appendEntry(.init(kind: .status, text: "[exit \(result.status)]"))
                }
            }
        }
        currentDirectory = session.currentDirectory
        history = session.history()
        refreshConfiguration()
        isExecuting = false
        executionTask = nil
    }

    private func append(_ result: RuneCommandResult) {
        appendOutput(stdout: result.stdout, stderr: result.stderr)
        if result.status != 0 {
            appendEntry(.init(kind: .status, text: "[exit \(result.status)]"))
        }
    }

    private func append(_ event: RuneExecutionEvent) {
        switch event.kind {
        case .output:
            appendOutput(stdout: event.stdout, stderr: event.stderr)
        case .status:
            if event.status != 0 {
                appendEntry(.init(kind: .status, text: "[exit \(event.status)]"))
            }
        }
        if !event.currentDirectory.isEmpty {
            currentDirectory = event.currentDirectory
        }
    }

    private func appendOutput(stdout value: String, stderr: String) {
        var stdout = value
        if let clearRange = stdout.range(of: Self.clearSequence, options: .backwards) {
            clearTranscript()
            stdout = String(stdout[clearRange.upperBound...])
        } else if let clearRange = stdout.range(of: Self.clearScreenControl, options: .backwards) {
            clearTranscript()
            stdout = String(stdout[clearRange.upperBound...])
        }
        stdout = stdout.replacingOccurrences(of: Self.cursorHomeControl, with: "")
        if !stdout.isEmpty {
            appendEntry(.init(kind: .stdout, text: stdout))
        }
        if !stderr.isEmpty {
            appendEntry(.init(kind: .stderr, text: stderr))
        }
    }

    private func appendEntry(_ entry: RuneTranscriptEntry) {
        entries.append(entry)
        transcriptBytes += entry.text.utf8.count
        while entries.count > Self.maximumTranscriptEntries
            || transcriptBytes > Self.maximumTranscriptBytes {
            guard let removed = entries.first else { break }
            transcriptBytes = max(0, transcriptBytes - removed.text.utf8.count)
            entries.removeFirst()
        }
    }

    private func clearTranscript() {
        entries.removeAll(keepingCapacity: true)
        transcriptBytes = 0
    }

    private func refreshConfiguration() {
        guard let session else { return }
        for line in session.configuration.split(separator: "\n") {
            let pair = line.split(separator: "=", maxSplits: 1).map(String.init)
            guard pair.count == 2, pair[0] == "font-size", let value = Double(pair[1]) else {
                if pair.count == 2, pair[0] == "theme", ["ink", "light", "ember"].contains(pair[1]) {
                    theme = pair[1]
                }
                continue
            }
            fontSize = CGFloat(min(max(value, 8), 32))
        }
    }

    public func previousHistory() {
        guard !history.isEmpty else { return }
        let current = historyCursor ?? history.count
        historyCursor = max(current - 1, 0)
        command = history[historyCursor ?? 0]
    }

    public func nextHistory() {
        guard let current = historyCursor else { return }
        let next = current + 1
        if next >= history.count {
            historyCursor = nil
            command = ""
        } else {
            historyCursor = next
            command = history[next]
        }
    }

    public var completionCandidates: [String] {
        session?.completionCandidates(for: command) ?? []
    }

    public func applyCompletion(_ candidate: String) {
        let tokenStart = command
            .indices
            .reversed()
            .first(where: { command[$0].isWhitespace })
            .map { command.index(after: $0) } ?? command.startIndex
        let prefix = String(command[..<tokenStart])
        let suffix = candidate.hasSuffix("/") ? "" : " "
        command = "\(prefix)\(candidate)\(suffix)"
    }
}

public struct RuneTerminalView: View {
    @StateObject private var model: RuneTerminalModel
    @FocusState private var inputFocused: Bool
    @State private var isImportingFolder = false

    public init(rootURL: URL? = nil, sessionID: String? = nil) {
        _model = StateObject(wrappedValue: RuneTerminalModel(rootURL: rootURL, sessionID: sessionID))
    }

    public var body: some View {
        let palette = RunePalette.forName(model.theme)

        ZStack {
            RuneBackground(palette: palette)

            VStack(spacing: 0) {
                HStack(spacing: 10) {
                    Circle()
                        .fill(palette.cyan)
                        .frame(width: 8, height: 8)
                        .shadow(color: palette.cyan.opacity(0.8), radius: 8)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("RUNE")
                            .font(.system(size: 12, weight: .bold, design: .rounded))
                            .tracking(2)
                            .foregroundStyle(palette.cyan)
                        Text("RUST CORE / LOCAL SESSION")
                            .font(.system(size: 10, weight: .medium, design: .monospaced))
                            .foregroundStyle(palette.muted)
                    }
                    Button {
                        isImportingFolder = true
                    } label: {
                        Image(systemName: "folder.badge.plus")
                            .font(.title3)
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(palette.cyan)
                    .accessibilityLabel("Open a folder")
                    .keyboardShortcut("o", modifiers: [.command])
                    if model.isExecuting {
                        Button {
                            model.cancel()
                        } label: {
                            Image(systemName: "stop.circle.fill")
                                .font(.title3)
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(palette.ember)
                        .accessibilityLabel("Cancel command")
                        .keyboardShortcut(".", modifiers: [.command])
                    }
                    Spacer()
                    VStack(alignment: .trailing, spacing: 2) {
                        Text(model.workspaceName)
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(palette.muted)
                        Text(model.currentDirectory)
                            .font(.system(size: 12, design: .monospaced))
                            .foregroundStyle(palette.foreground)
                            .lineLimit(1)
                        Text("\(model.entries.count) events")
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(palette.muted)
                    }
                }
                .padding(.horizontal, 18)
                .padding(.vertical, 14)
                .background(palette.panel.opacity(0.94))
                .overlay(alignment: .bottom) {
                    Rectangle()
                        .fill(palette.cyan.opacity(0.22))
                        .frame(height: 1)
                }

                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 9) {
                            if let initializationError = model.initializationError {
                                Text(initializationError)
                                    .foregroundStyle(palette.error)
                                    .textSelection(.enabled)
                            }
                            ForEach(model.entries) { entry in
                                RuneANSIText(
                                    text: entry.text,
                                    defaultColor: color(for: entry.kind, palette: palette)
                                )
                                    .font(.system(size: model.fontSize, design: .monospaced))
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .textSelection(.enabled)
                                    .id(entry.id)
                            }
                        }
                        .padding(18)
                    }
                    .scrollDismissesKeyboard(.interactively)
                    .onChange(of: model.entries.count) { _, _ in
                        if let last = model.entries.last {
                            withAnimation(.easeOut(duration: 0.15)) {
                                proxy.scrollTo(last.id, anchor: .bottom)
                            }
                        }
                    }
                }

                if !model.completionCandidates.isEmpty {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            ForEach(model.completionCandidates, id: \.self) { candidate in
                                RuneCompletionButton(
                                    candidate: candidate,
                                    fontSize: model.fontSize,
                                    foreground: palette.foreground,
                                    tint: palette.cyan
                                ) {
                                    model.applyCompletion(candidate)
                                    inputFocused = true
                                }
                            }
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 7)
                    }
                }

                HStack(alignment: .bottom, spacing: 10) {
                    Text("\(model.currentDirectory) ›")
                        .font(.system(size: 12, weight: .medium, design: .monospaced))
                        .foregroundStyle(palette.cyan)
                        .lineLimit(1)
                    TextField("Enter a Rune command", text: $model.command, axis: .vertical)
                        .font(.system(size: model.fontSize, design: .monospaced))
                        .foregroundStyle(palette.foreground)
                        .textFieldStyle(.plain)
                        .lineLimit(1...4)
                        .focused($inputFocused)
                        .autocorrectionDisabled(true)
                        .textInputAutocapitalization(.never)
                        .disabled(model.isExecuting)
                        .onSubmit {
                            model.submit()
                            inputFocused = true
                        }
                        .accessibilityLabel("Command input")
                    Button {
                        model.previousHistory()
                        inputFocused = true
                    } label: {
                        Image(systemName: "chevron.up")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(palette.muted)
                    .accessibilityLabel("Previous command")
                    .disabled(model.isExecuting)
                    .keyboardShortcut(.upArrow, modifiers: [.command])
                    Button {
                        model.nextHistory()
                        inputFocused = true
                    } label: {
                        Image(systemName: "chevron.down")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(palette.muted)
                    .accessibilityLabel("Next command")
                    .disabled(model.isExecuting)
                    .keyboardShortcut(.downArrow, modifiers: [.command])
                    Button {
                        model.submit()
                        inputFocused = true
                    } label: {
                        Image(systemName: "arrow.up.circle.fill")
                            .font(.title2)
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(palette.cyan)
                    .accessibilityLabel("Execute command")
                    .disabled(model.isExecuting)
                    .keyboardShortcut(.return, modifiers: [.command])
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .background(palette.panel.opacity(0.97))
                .overlay(alignment: .top) {
                    Rectangle()
                        .fill(palette.ember.opacity(0.5))
                        .frame(height: 1)
                }
            }
        }
        .preferredColorScheme(model.theme == "light" ? .light : .dark)
        .fileImporter(isPresented: $isImportingFolder, allowedContentTypes: [.folder]) { result in
            switch result {
            case .success(let url):
                model.openFolder(url)
            case .failure(let error):
                model.reportFolderImportError(error)
            }
        }
        .onAppear { inputFocused = true }
    }

    private func color(for kind: RuneTranscriptEntry.Kind, palette: RunePalette) -> Color {
        switch kind {
        case .command: return palette.cyan
        case .stdout: return palette.foreground
        case .stderr: return palette.ember
        case .status: return palette.muted
        }
    }
}

private struct RuneCompletionButton: View {
    let candidate: String
    let fontSize: CGFloat
    let foreground: Color
    let tint: Color
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(verbatim: candidate)
                .font(.system(
                    size: max(fontSize - 2, 11),
                    weight: .medium,
                    design: .monospaced
                ))
                .foregroundStyle(foreground)
                .padding(.horizontal, 11)
                .padding(.vertical, 8)
                .background(tint.opacity(0.12))
                .overlay {
                    Capsule()
                        .stroke(tint.opacity(0.45), lineWidth: 1)
                }
                .clipShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Completion option")
        .accessibilityValue(Text(verbatim: candidate))
    }
}

private struct RunePalette {
    let background: Color
    let panel: Color
    let foreground: Color
    let muted: Color
    let cyan: Color
    let ember: Color
    let error: Color

    static func forName(_ name: String) -> RunePalette {
        switch name {
        case "light":
            return RunePalette(
                background: Color(red: 0.94, green: 0.96, blue: 0.98),
                panel: Color.white.opacity(0.94),
                foreground: Color(red: 0.10, green: 0.14, blue: 0.19),
                muted: Color(red: 0.31, green: 0.39, blue: 0.47),
                cyan: Color(red: 0.02, green: 0.42, blue: 0.58),
                ember: Color(red: 0.78, green: 0.26, blue: 0.06),
                error: Color(red: 0.72, green: 0.08, blue: 0.16)
            )
        case "ember":
            return RunePalette(
                background: Color(red: 0.08, green: 0.035, blue: 0.018),
                panel: Color(red: 0.15, green: 0.065, blue: 0.03),
                foreground: Color(red: 1.0, green: 0.89, blue: 0.72),
                muted: Color(red: 0.66, green: 0.44, blue: 0.28),
                cyan: Color(red: 1.0, green: 0.68, blue: 0.24),
                ember: Color(red: 1.0, green: 0.32, blue: 0.12),
                error: Color(red: 1.0, green: 0.26, blue: 0.2)
            )
        default:
            return RunePalette(
                background: Color(red: 0.018, green: 0.024, blue: 0.035),
                panel: Color(red: 0.045, green: 0.058, blue: 0.078),
                foreground: Color(red: 0.88, green: 0.93, blue: 0.95),
                muted: Color(red: 0.43, green: 0.52, blue: 0.58),
                cyan: Color(red: 0.18, green: 0.88, blue: 0.93),
                ember: Color(red: 1.0, green: 0.54, blue: 0.25),
                error: Color(red: 1.0, green: 0.34, blue: 0.4)
            )
        }
    }
}

private struct RuneBackground: View {
    let palette: RunePalette

    var body: some View {
        ZStack {
            palette.background
            RadialGradient(
                colors: [palette.cyan.opacity(0.13), .clear],
                center: .topLeading,
                startRadius: 0,
                endRadius: 500
            )
            RadialGradient(
                colors: [palette.ember.opacity(0.08), .clear],
                center: .bottomTrailing,
                startRadius: 0,
                endRadius: 420
            )
        }
        .ignoresSafeArea()
    }
}

@main
public struct RuneIOSApp: App {
    public init() {}

    public var body: some Scene {
        WindowGroup(id: "rune-terminal", for: RuneWindowRoute.self) { route in
            RuneWorkspaceView(sessionID: route.wrappedValue?.sessionID)
        }
    }
}
