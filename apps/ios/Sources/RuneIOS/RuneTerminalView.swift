import SwiftUI
import Combine

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
    @Published public private(set) var entries: [RuneTranscriptEntry] = []
    @Published public var command = ""
    @Published public private(set) var currentDirectory = "~"
    @Published public private(set) var initializationError: String?

    private let session: RuneFFISession?
    private var history: [String] = []
    private var commandNames: [String] = []
    private var historyCursor: Int?

    public init(rootURL: URL? = nil) {
        let root = rootURL ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
        guard let root else {
            session = nil
            initializationError = "Rune could not locate the app Documents directory."
            return
        }
        do {
            session = try RuneFFISession(rootURL: root)
            currentDirectory = session?.currentDirectory ?? "~"
            history = session?.history() ?? []
            commandNames = session?.commands() ?? []
        } catch {
            session = nil
            initializationError = error.localizedDescription
        }
    }

    public func submit() {
        let line = command.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !line.isEmpty else { return }
        entries.append(.init(kind: .command, text: "\(currentDirectory) $ \(line)"))
        command = ""
        historyCursor = nil
        guard let session else {
            entries.append(.init(kind: .stderr, text: initializationError ?? "Rune session unavailable."))
            return
        }
        let result = session.execute(line)
        if !result.stdout.isEmpty {
            entries.append(.init(kind: .stdout, text: result.stdout))
        }
        if !result.stderr.isEmpty {
            entries.append(.init(kind: .stderr, text: result.stderr))
        }
        if result.status != 0 {
            entries.append(.init(kind: .status, text: "[exit \(result.status)]"))
        }
        currentDirectory = session.currentDirectory
        history = session.history()
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
        let trimmed = command.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty,
              !trimmed.contains(where: { character in
                  character.isWhitespace || "|;&<>".contains(character)
              })
        else {
            return []
        }
        return commandNames
            .filter { $0.hasPrefix(trimmed) && $0 != trimmed }
            .prefix(8)
            .map { $0 }
    }

    public func applyCompletion(_ candidate: String) {
        let leadingWhitespace = String(command.prefix(while: { $0.isWhitespace }))
        command = "\(leadingWhitespace)\(candidate) "
    }
}

public struct RuneTerminalView: View {
    @StateObject private var model: RuneTerminalModel

    public init(rootURL: URL? = nil) {
        _model = StateObject(wrappedValue: RuneTerminalModel(rootURL: rootURL))
    }

    public var body: some View {
        VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("RUNE")
                        .font(.system(size: 12, weight: .bold, design: .rounded))
                        .foregroundStyle(.secondary)
                    Text("Local session")
                        .font(.headline)
                }
                Spacer()
                Text(model.currentDirectory)
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 14)
            .background(.ultraThinMaterial)

            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 9) {
                        if let initializationError = model.initializationError {
                            Text(initializationError)
                                .foregroundStyle(.red)
                                .textSelection(.enabled)
                        }
                        ForEach(model.entries) { entry in
                            Text(entry.text)
                                .font(.system(size: 15, design: .monospaced))
                                .foregroundStyle(color(for: entry.kind))
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
                            Button(candidate) {
                                model.applyCompletion(candidate)
                            }
                            .buttonStyle(.bordered)
                            .font(.system(size: 13, design: .monospaced))
                            .accessibilityLabel("Complete with \(candidate)")
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 6)
                }
            }

            HStack(alignment: .bottom, spacing: 10) {
                Text("›")
                    .font(.system(size: 22, weight: .semibold, design: .monospaced))
                    .foregroundStyle(.tint)
                TextField("Enter a Rune command", text: $model.command, axis: .vertical)
                    .font(.system(size: 15, design: .monospaced))
                    .textFieldStyle(.plain)
                    .lineLimit(1...4)
                    .onSubmit(model.submit)
                    .accessibilityLabel("Command input")
                Button(action: model.previousHistory) {
                    Image(systemName: "chevron.up")
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Previous command")
                Button(action: model.nextHistory) {
                    Image(systemName: "chevron.down")
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Next command")
                Button(action: model.submit) {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title2)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Execute command")
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .background(.regularMaterial)
        }
        .background(Color(red: 0.035, green: 0.045, blue: 0.06))
        .preferredColorScheme(.dark)
    }

    private func color(for kind: RuneTranscriptEntry.Kind) -> Color {
        switch kind {
        case .command: return .cyan
        case .stdout: return .white
        case .stderr: return .orange
        case .status: return .secondary
        }
    }
}

public struct RuneIOSApp: App {
    public init() {}

    public var body: some Scene {
        WindowGroup {
            RuneTerminalView()
        }
    }
}
