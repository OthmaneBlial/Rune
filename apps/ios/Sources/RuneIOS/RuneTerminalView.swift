import SwiftUI
import Combine
import UniformTypeIdentifiers
#if canImport(UIKit)
import UIKit
#endif

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
    let ansiSegments: [RuneANSISegment]

    public init(kind: Kind, text: String) {
        self.kind = kind
        self.text = text
        ansiSegments = RuneANSIRenderer.segments(from: text)
    }
}

@MainActor
public final class RuneTerminalModel: ObservableObject {
    private static let clearSequence = "\u{1b}[2J\u{1b}[H"
    private static let clearScreenControl = "\u{1b}[2J"
    private static let cursorHomeControl = "\u{1b}[H"
    private static let maximumTranscriptEntries = 8_192
    private static let maximumTranscriptBytes = 8 * 1024 * 1024
    private static let defaultScrollbackLimit = 4_096
    private static let minimumScrollbackLimit = 128

    @Published public private(set) var entries: [RuneTranscriptEntry] = []
    @Published public var command = ""
    @Published public private(set) var currentDirectory = "~"
    @Published public private(set) var workspaceName = "Documents"
    @Published public private(set) var fontSize: CGFloat = 15
    @Published public private(set) var font = "monospaced"
    @Published public private(set) var scrollbackLimit = Self.defaultScrollbackLimit
    @Published public private(set) var toolbarVisible = true
    @Published public private(set) var historyRedaction = true
    @Published public private(set) var environmentPersistence = false
    @Published public private(set) var theme = "ink"
    @Published public private(set) var cursorColor = "cyan"
    @Published public private(set) var cursorShape = "bar"
    @Published public private(set) var background = "auto"
    @Published public private(set) var foreground = "auto"
    @Published public private(set) var initializationError: String?
    @Published public private(set) var isExecuting = false
    @Published public private(set) var historyMatches: [String] = []

    private var session: RuneFFISession?
    private var scopedFolder: RuneScopedFolder?
    private let sessionID: String?
    private var history: [String] = []
    private var historyCursor: Int?
    private var executionTask: Task<Void, Never>?
    private var transcriptBytes = 0

    public init(rootURL: URL? = nil, sessionID: String? = nil) {
        self.sessionID = sessionID
        let documentsURL = FileManager.default
            .urls(for: .documentDirectory, in: .userDomainMask)
            .first
        let root = rootURL ?? documentsURL
        let libraryURL = rootURL == nil
            ? FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask).first
            : nil
        let temporaryURL = rootURL == nil ? FileManager.default.temporaryDirectory : nil
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
            session = try RuneFFISession(
                rootURL: root,
                libraryURL: libraryURL,
                temporaryURL: temporaryURL,
                sessionID: sessionID
            )
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
            historyMatches = []
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
        historyMatches = []
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

    public func insertText(_ value: String) {
        guard !isExecuting else { return }
        command.append(contentsOf: value)
    }

    public func acceptFirstCompletion() {
        guard !isExecuting else { return }
        if let candidate = completionCandidates.first {
            applyCompletion(candidate)
        } else {
            insertText("\t")
        }
    }

    /// Clears only the native transcript presentation. The Rust session and
    /// persisted history remain unchanged; the `clear` command remains the
    /// portable shell control for scripted callers.
    public func clearDisplay() {
        guard !isExecuting else { return }
        clearTranscript()
    }

    /// Updates a validated Rust-owned setting without routing the change
    /// through shell text or adding it to command history.
    public func setConfiguration(key: String, value: String) {
        guard !isExecuting else { return }
        guard let session else {
            initializationError = "Rune session unavailable."
            return
        }
        let result = session.setConfiguration(key: key, value: value)
        if result.status != 0 {
            append(result)
        }
        refreshConfiguration()
    }

    /// Restores Rust-owned settings to their defaults without adding a shell
    /// command to history.
    public func resetConfiguration() {
        guard !isExecuting else { return }
        guard let session else {
            initializationError = "Rune session unavailable."
            return
        }
        let result = session.resetConfiguration()
        if result.status != 0 {
            append(result)
        }
        refreshConfiguration()
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
        trimTranscript()
    }

    private func trimTranscript() {
        let entryLimit = Swift.min(Self.maximumTranscriptEntries, scrollbackLimit)
        while entries.count > entryLimit || transcriptBytes > Self.maximumTranscriptBytes {
            guard let removed = entries.first else { break }
            transcriptBytes = Swift.max(0, transcriptBytes - removed.text.utf8.count)
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
            guard pair.count == 2 else { continue }
            switch pair[0] {
            case "font-size":
                if let value = Double(pair[1]) {
                    fontSize = CGFloat(Swift.min(Swift.max(value, 8), 32))
                }
            case "font":
                if ["monospaced", "system", "rounded"].contains(pair[1]) {
                    font = pair[1]
                }
            case "scrollback-limit":
                if let value = Int(pair[1]) {
                    scrollbackLimit = Swift.min(
                        Swift.max(value, Self.minimumScrollbackLimit),
                        Self.maximumTranscriptEntries
                    )
                }
            case "toolbar-visible":
                if ["true", "1"].contains(pair[1]) {
                    toolbarVisible = true
                } else if ["false", "0"].contains(pair[1]) {
                    toolbarVisible = false
                }
            case "history-redaction":
                if ["true", "1"].contains(pair[1]) {
                    historyRedaction = true
                } else if ["false", "0"].contains(pair[1]) {
                    historyRedaction = false
                }
            case "environment-persistence":
                if ["true", "1"].contains(pair[1]) {
                    environmentPersistence = true
                } else if ["false", "0"].contains(pair[1]) {
                    environmentPersistence = false
                }
            case "theme":
                if ["ink", "light", "ember"].contains(pair[1]) {
                    theme = pair[1]
                }
            case "cursor-color":
                if ["cyan", "ember", "foreground"].contains(pair[1]) {
                    cursorColor = pair[1]
                }
            case "cursor-shape":
                if ["bar", "block", "underline"].contains(pair[1]) {
                    cursorShape = pair[1]
                }
            case "background":
                if ["auto", "black", "white", "slate"].contains(pair[1]) {
                    background = pair[1]
                }
            case "foreground":
                if ["auto", "black", "white", "cyan", "ember"].contains(pair[1]) {
                    foreground = pair[1]
                }
            default:
                continue
            }
        }
        trimTranscript()
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

    /// Refreshes newest-first history matches through the Rust-owned session
    /// without recording a search command.
    public func updateHistorySearch(_ query: String) {
        guard !isExecuting else { return }
        guard !query.isEmpty else {
            historyMatches = []
            return
        }
        historyMatches = session?.historySearch(query) ?? []
    }

    /// Loads one Rust-returned history match into the command editor.
    public func selectHistoryMatch(_ value: String) {
        guard !isExecuting else { return }
        command = value
        historyCursor = nil
        historyMatches = []
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
    @State private var isShowingSettings = false
    @State private var isShowingHistorySearch = false
    @State private var historyQuery = ""

    public init(rootURL: URL? = nil, sessionID: String? = nil) {
        _model = StateObject(wrappedValue: RuneTerminalModel(rootURL: rootURL, sessionID: sessionID))
    }

    public var body: some View {
        let palette = RunePalette.forName(
            model.theme,
            background: model.background,
            foreground: model.foreground
        )

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
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel("Rune terminal")
                    .accessibilityValue("Rust core, local session")
                    Button {
                        isImportingFolder = true
                    } label: {
                        Image(systemName: "folder.badge.plus")
                            .font(.title3)
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(palette.cyan)
                    .accessibilityLabel("Open a folder")
                    .accessibilityHint("Choose an external folder for a confined Rune session.")
                    .accessibilityIdentifier("rune.openFolder")
                    .keyboardShortcut("o", modifiers: [.command])
                    Button {
                        isShowingSettings = true
                    } label: {
                        Image(systemName: "gearshape")
                            .font(.title3)
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(palette.cyan)
                    .accessibilityLabel("Terminal settings")
                    .accessibilityHint("Configure terminal appearance and scrollback.")
                    .accessibilityIdentifier("rune.settings")
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
                        .accessibilityHint("Request cooperative cancellation from the Rust session.")
                        .accessibilityIdentifier("rune.cancel")
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
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel("Session status")
                    .accessibilityValue(
                        "\(model.workspaceName), directory \(model.currentDirectory), "
                            + "\(model.entries.count) events"
                    )
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
                                    segments: entry.ansiSegments,
                                    defaultColor: color(for: entry.kind, palette: palette),
                                    defaultBackground: palette.background
                                )
                                    .font(terminalFont(size: model.fontSize))
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .textSelection(.enabled)
                                    .accessibilityElement(children: .ignore)
                                    .accessibilityLabel(accessibilityLabel(for: entry.kind))
                                    .accessibilityValue(Text(verbatim: entry.text))
                                    .id(entry.id)
                            }
                        }
                        .padding(18)
                    }
                    .accessibilityIdentifier("rune.transcript")
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
                        .accessibilityElement(children: .contain)
                        .accessibilityLabel("Command completions")
                    }
                }

                if isShowingHistorySearch {
                    RuneHistorySearchPanel(
                        model: model,
                        query: $historyQuery,
                        palette: palette,
                        close: closeHistorySearch,
                        focusInput: { inputFocused = true }
                    )
                }

                if model.toolbarVisible {
                    RuneInputToolbar(model: model) {
                        inputFocused = true
                    }
                }

                HStack(alignment: .bottom, spacing: 10) {
                    Text("\(model.currentDirectory) ›")
                        .font(.system(size: 12, weight: .medium, design: .monospaced))
                        .foregroundStyle(palette.cyan)
                        .lineLimit(1)
#if canImport(UIKit)
                    RuneUIKitCommandEditor(
                        text: $model.command,
                        isFocused: Binding(
                            get: { inputFocused },
                            set: { inputFocused = $0 }
                        ),
                        fontSize: model.fontSize,
                        fontDesign: model.font,
                        foreground: model.foreground,
                        cursorColor: model.cursorColor,
                        cursorShape: model.cursorShape,
                        onSubmit: {
                            model.submit()
                            inputFocused = true
                        }
                    )
                    .frame(minHeight: 22, maxHeight: 100)
#else
                    TextField("Enter a Rune command", text: $model.command, axis: .vertical)
                        .font(terminalFont(size: model.fontSize))
                        .foregroundStyle(palette.foreground)
                        .tint(palette.cursorColor(named: model.cursorColor))
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
#endif
                        .accessibilityLabel("Command input")
                        .accessibilityHint("Enter a Rust-backed Rune command and submit it.")
                        .accessibilityIdentifier("rune.commandInput")
                    Button {
                        model.previousHistory()
                        inputFocused = true
                    } label: {
                        Image(systemName: "chevron.up")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(palette.muted)
                    .accessibilityLabel("Previous command")
                    .accessibilityHint("Load the previous command from this session's history.")
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
                    .accessibilityHint("Load the next command from this session's history.")
                    .disabled(model.isExecuting)
                    .keyboardShortcut(.downArrow, modifiers: [.command])
                    Button {
                        if isShowingHistorySearch {
                            closeHistorySearch()
                        } else {
                            isShowingHistorySearch = true
                            historyQuery = ""
                            model.updateHistorySearch("")
                            inputFocused = true
                        }
                    } label: {
                        Image(systemName: "magnifyingglass")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(palette.muted)
                    .accessibilityLabel("Search command history")
                    .accessibilityHint("Find a previous command through the Rust history boundary.")
                    .accessibilityIdentifier("rune.historySearch")
                    .disabled(model.isExecuting)
                    .keyboardShortcut("r", modifiers: [.control])
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
                    .accessibilityHint("Send the command to the Rust session.")
                    .accessibilityIdentifier("rune.execute")
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
        .sheet(isPresented: $isShowingSettings) {
            RuneSettingsView(model: model)
        }
        .onAppear { inputFocused = true }
    }

    private func closeHistorySearch() {
        isShowingHistorySearch = false
        historyQuery = ""
        model.updateHistorySearch("")
    }

    private func color(for kind: RuneTranscriptEntry.Kind, palette: RunePalette) -> Color {
        switch kind {
        case .command: return palette.cyan
        case .stdout: return palette.foreground
        case .stderr: return palette.ember
        case .status: return palette.muted
        }
    }

    private func accessibilityLabel(for kind: RuneTranscriptEntry.Kind) -> String {
        switch kind {
        case .command: return "Command"
        case .stdout: return "Command output"
        case .stderr: return "Command error output"
        case .status: return "Command status"
        }
    }

    private func terminalFont(size: CGFloat) -> Font {
        let design: Font.Design
        switch model.font {
        case "system": design = .default
        case "rounded": design = .rounded
        default: design = .monospaced
        }
        return .system(size: size, design: design)
    }
}

#if canImport(UIKit)
private final class RuneCursorTextView: UITextView {
    var cursorShape = "bar" {
        didSet { setNeedsDisplay() }
    }

    override func caretRect(for position: UITextPosition) -> CGRect {
        var rect = super.caretRect(for: position)
        guard !rect.isNull, !rect.isInfinite, rect.height > 0 else {
            return rect
        }
        switch cursorShape {
        case "block":
            rect.size.width = max(rect.width, rect.height * 0.6)
        case "underline":
            rect.origin.y = max(rect.origin.y, rect.maxY - 2)
            rect.size.height = 2
        default:
            rect.size.width = max(rect.width, 2)
        }
        return rect
    }
}

private struct RuneUIKitCommandEditor: UIViewRepresentable {
    @Binding var text: String
    @Binding var isFocused: Bool
    let fontSize: CGFloat
    let fontDesign: String
    let foreground: String
    let cursorColor: String
    let cursorShape: String
    let onSubmit: () -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(text: $text, onSubmit: onSubmit)
    }

    func makeUIView(context: Context) -> RuneCursorTextView {
        let view = RuneCursorTextView()
        view.delegate = context.coordinator
        view.backgroundColor = .clear
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.textContainer.maximumNumberOfLines = 4
        view.textContainer.lineBreakMode = .byWordWrapping
        view.isScrollEnabled = false
        view.autocorrectionType = .no
        view.autocapitalizationType = .none
        view.font = resolvedFont()
        view.textColor = resolvedForegroundColor()
        view.tintColor = resolvedCursorColor()
        view.cursorShape = cursorShape
        view.setContentHuggingPriority(.defaultLow, for: .horizontal)
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return view
    }

    func updateUIView(_ view: RuneCursorTextView, context: Context) {
        if view.text != text {
            view.text = text
        }
        view.font = resolvedFont()
        view.textColor = resolvedForegroundColor()
        view.tintColor = resolvedCursorColor()
        view.cursorShape = cursorShape
        view.invalidateIntrinsicContentSize()
        if isFocused, !view.isFirstResponder {
            view.becomeFirstResponder()
        } else if !isFocused, view.isFirstResponder {
            view.resignFirstResponder()
        }
    }

    private func resolvedFont() -> UIFont {
        if fontDesign == "monospaced" {
            return .monospacedSystemFont(ofSize: fontSize, weight: .regular)
        }
        return .systemFont(ofSize: fontSize)
    }

    private func resolvedForegroundColor() -> UIColor {
        switch foreground {
        case "black": return .black
        case "white": return .white
        case "cyan": return .systemTeal
        case "ember": return .systemOrange
        default: return .label
        }
    }

    private func resolvedCursorColor() -> UIColor {
        switch cursorColor {
        case "ember": return .systemOrange
        case "foreground": return resolvedForegroundColor()
        default: return .systemTeal
        }
    }

    final class Coordinator: NSObject, UITextViewDelegate {
        @Binding var text: String
        let onSubmit: () -> Void

        init(text: Binding<String>, onSubmit: @escaping () -> Void) {
            _text = text
            self.onSubmit = onSubmit
        }

        func textViewDidChange(_ textView: UITextView) {
            text = textView.text
        }

        func textView(
            _ textView: UITextView,
            shouldChangeTextIn range: NSRange,
            replacementText replacement: String
        ) -> Bool {
            guard replacement == "\n" else { return true }
            onSubmit()
            return false
        }
    }
}
#endif

private struct RuneSettingsView: View {
    @ObservedObject var model: RuneTerminalModel
    @Environment(\.dismiss) private var dismiss

    private let themes = ["ink", "light", "ember"]
    private let cursorColors = ["cyan", "ember", "foreground"]
    private let cursorShapes = ["bar", "block", "underline"]
    private let fonts = ["monospaced", "system", "rounded"]
    private let backgrounds = ["auto", "black", "white", "slate"]
    private let foregrounds = ["auto", "black", "white", "cyan", "ember"]

    var body: some View {
        NavigationStack {
            Form {
                Section("Terminal") {
                    Toggle(
                        "Show input toolbar",
                        isOn: Binding(
                            get: { model.toolbarVisible },
                            set: {
                                model.setConfiguration(
                                    key: "toolbar-visible",
                                    value: $0 ? "true" : "false"
                                )
                            }
                        )
                    )

                    Toggle(
                        "Redact secrets in history",
                        isOn: Binding(
                            get: { model.historyRedaction },
                            set: {
                                model.setConfiguration(
                                    key: "history-redaction",
                                    value: $0 ? "true" : "false"
                                )
                            }
                        )
                    )
                    .accessibilityHint(
                        "When enabled, detected environment assignments and network commands are replaced before history is stored."
                    )

                    Toggle(
                        "Persist session environment",
                        isOn: Binding(
                            get: { model.environmentPersistence },
                            set: {
                                model.setConfiguration(
                                    key: "environment-persistence",
                                    value: $0 ? "true" : "false"
                                )
                            }
                        )
                    )
                    .accessibilityHint(
                        "When enabled, user-defined environment values are restored with this session. Do not use it for secrets."
                    )

                    HStack {
                        Text("Font size")
                        Spacer()
                        Button {
                            adjustFontSize(by: -1)
                        } label: {
                            Image(systemName: "minus.circle")
                        }
                        .accessibilityLabel("Decrease font size")
                        Text("\(Int(model.fontSize)) pt")
                            .monospacedDigit()
                            .frame(minWidth: 58)
                        Button {
                            adjustFontSize(by: 1)
                        } label: {
                            Image(systemName: "plus.circle")
                        }
                        .accessibilityLabel("Increase font size")
                    }

                    Picker(
                        "Theme",
                        selection: Binding(
                            get: { model.theme },
                            set: { model.setConfiguration(key: "theme", value: $0) }
                        )
                    ) {
                        ForEach(themes, id: \.self) { theme in
                            Text(theme.capitalized).tag(theme)
                        }
                    }

                    Picker(
                        "Font",
                        selection: Binding(
                            get: { model.font },
                            set: { model.setConfiguration(key: "font", value: $0) }
                        )
                    ) {
                        ForEach(fonts, id: \.self) { font in
                            Text(font.capitalized).tag(font)
                        }
                    }

                    Picker(
                        "Cursor color",
                        selection: Binding(
                            get: { model.cursorColor },
                            set: { model.setConfiguration(key: "cursor-color", value: $0) }
                        )
                    ) {
                        ForEach(cursorColors, id: \.self) { color in
                            Text(color.capitalized).tag(color)
                        }
                    }

                    Picker(
                        "Cursor shape",
                        selection: Binding(
                            get: { model.cursorShape },
                            set: { model.setConfiguration(key: "cursor-shape", value: $0) }
                        )
                    ) {
                        ForEach(cursorShapes, id: \.self) { shape in
                            Text(shape.capitalized).tag(shape)
                        }
                    }

                    Picker(
                        "Background",
                        selection: Binding(
                            get: { model.background },
                            set: { model.setConfiguration(key: "background", value: $0) }
                        )
                    ) {
                        ForEach(backgrounds, id: \.self) { color in
                            Text(color.capitalized).tag(color)
                        }
                    }

                    Picker(
                        "Foreground",
                        selection: Binding(
                            get: { model.foreground },
                            set: { model.setConfiguration(key: "foreground", value: $0) }
                        )
                    ) {
                        ForEach(foregrounds, id: \.self) { color in
                            Text(color.capitalized).tag(color)
                        }
                    }

                    HStack {
                        Text("Scrollback")
                        Spacer()
                        Button {
                            adjustScrollback(by: -256)
                        } label: {
                            Image(systemName: "minus.circle")
                        }
                        .accessibilityLabel("Decrease scrollback")
                        Text("\(model.scrollbackLimit)")
                            .monospacedDigit()
                            .frame(minWidth: 58)
                        Button {
                            adjustScrollback(by: 256)
                        } label: {
                            Image(systemName: "plus.circle")
                        }
                        .accessibilityLabel("Increase scrollback")
                    }
                }

                Section {
                    Button("Reset Rust settings", role: .destructive) {
                        model.resetConfiguration()
                    }
                } footer: {
                    Text("Settings are validated and persisted by the Rust session. Environment persistence is off by default and may store exported values.")
                }
            }
            .navigationTitle("Rune Settings")
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") {
                        dismiss()
                    }
                }
            }
        }
        .preferredColorScheme(model.theme == "light" ? .light : .dark)
    }

    private func adjustFontSize(by delta: Int) {
        let value = min(max(Int(model.fontSize) + delta, 8), 32)
        model.setConfiguration(key: "font-size", value: String(value))
    }

    private func adjustScrollback(by delta: Int) {
        let value = min(max(model.scrollbackLimit + delta, 128), 8_192)
        model.setConfiguration(key: "scrollback-limit", value: String(value))
    }
}

private struct RuneHistorySearchPanel: View {
    @ObservedObject var model: RuneTerminalModel
    @Binding var query: String
    let palette: RunePalette
    let close: () -> Void
    let focusInput: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(palette.cyan)
                TextField("Search command history", text: $query)
                    .font(.system(size: 13, design: .monospaced))
                    .foregroundStyle(palette.foreground)
                    .tint(palette.cursorColor(named: model.cursorColor))
                    .textFieldStyle(.plain)
                    .autocorrectionDisabled(true)
                    .textInputAutocapitalization(.never)
                    .onChange(of: query) { _, value in
                        model.updateHistorySearch(value)
                    }
                    .accessibilityLabel("History search query")
                    .accessibilityHint("Search previous Rust session commands by text.")
                    .accessibilityIdentifier("rune.historySearchInput")
                Button(action: close) {
                    Image(systemName: "xmark.circle.fill")
                }
                .buttonStyle(.plain)
                .foregroundStyle(palette.muted)
                .accessibilityLabel("Close history search")
            }

            if model.historyMatches.isEmpty {
                Text(query.isEmpty ? "Type to search recent commands" : "No matching commands")
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(palette.muted)
                    .accessibilityLabel(query.isEmpty ? "History search is empty" : "No matching commands")
            } else {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(Array(model.historyMatches.enumerated()), id: \.offset) { _, match in
                            Button {
                                model.selectHistoryMatch(match)
                                close()
                                focusInput()
                            } label: {
                                Text(verbatim: match)
                                    .font(.system(size: 12, design: .monospaced))
                                    .foregroundStyle(palette.foreground)
                                    .lineLimit(1)
                                    .padding(.horizontal, 10)
                                    .padding(.vertical, 7)
                                    .background(palette.cyan.opacity(0.12))
                                    .clipShape(Capsule())
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel("History match")
                            .accessibilityValue(Text(verbatim: match))
                        }
                    }
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background(palette.panel.opacity(0.95))
        .overlay(alignment: .top) {
            Rectangle()
                .fill(palette.cyan.opacity(0.22))
                .frame(height: 1)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Command history search")
    }
}

private struct RuneInputToolbar: View {
    @ObservedObject var model: RuneTerminalModel
    let focusInput: () -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                RuneToolbarButton(title: "Tab", systemImage: "arrow.right.to.line") {
                    model.acceptFirstCompletion()
                    focusInput()
                }
                RuneToolbarButton(title: "Esc", systemImage: "escape") {
                    model.insertText("\u{1b}")
                    focusInput()
                }
                RuneToolbarButton(title: "Ctrl-C", systemImage: "xmark.circle") {
                    model.cancel()
                    focusInput()
                }
                RuneToolbarButton(title: "Clear", systemImage: "clear") {
                    model.clearDisplay()
                    focusInput()
                }
                PasteButton(payloadType: String.self) { values in
                    model.insertText(values.joined())
                    focusInput()
                }
                .labelStyle(.titleAndIcon)
                .buttonStyle(.bordered)
                .accessibilityLabel("Paste text")
                .accessibilityHint("Insert text from the clipboard into the command input.")
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 7)
        }
    }
}

private struct RuneToolbarButton: View {
    let title: String
    let systemImage: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Label(title, systemImage: systemImage)
                .font(.system(size: 12, weight: .medium, design: .monospaced))
        }
        .buttonStyle(.bordered)
        .accessibilityLabel(title)
        .accessibilityHint("Apply \(title) to the command input.")
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

    func cursorColor(named name: String) -> Color {
        switch name {
        case "ember": return ember
        case "foreground": return foreground
        default: return cyan
        }
    }

    func overriding(background name: String, foreground foregroundName: String) -> RunePalette {
        let backgroundColor: Color
        let panelColor: Color
        switch name {
        case "black":
            backgroundColor = .black
            panelColor = Color.black.opacity(0.94)
        case "white":
            backgroundColor = .white
            panelColor = Color.white.opacity(0.94)
        case "slate":
            backgroundColor = Color(red: 0.08, green: 0.11, blue: 0.15)
            panelColor = Color(red: 0.12, green: 0.16, blue: 0.21).opacity(0.96)
        default:
            backgroundColor = background
            panelColor = panel
        }
        let foregroundColor: Color
        switch foregroundName {
        case "black": foregroundColor = .black
        case "white": foregroundColor = .white
        case "cyan": foregroundColor = cyan
        case "ember": foregroundColor = ember
        default: foregroundColor = foreground
        }
        return RunePalette(
            background: backgroundColor,
            panel: panelColor,
            foreground: foregroundColor,
            muted: muted,
            cyan: cyan,
            ember: ember,
            error: error
        )
    }

    static func forName(
        _ name: String,
        background: String,
        foreground: String
    ) -> RunePalette {
        forName(name).overriding(background: background, foreground: foreground)
    }

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
