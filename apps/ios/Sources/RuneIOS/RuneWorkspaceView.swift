import SwiftUI
import Foundation

private struct RuneWorkspaceTab: Identifiable, Hashable {
    let id: UUID
    let title: String
    let rootURL: URL?
    let sessionID: String?
}

/// Source-only tab container. Each tab creates its own Rust session namespace;
/// the terminal view remains responsible for command rendering and input.
public struct RuneWorkspaceView: View {
    @State private var tabs: [RuneWorkspaceTab]
    @State private var selectedTabID: UUID
    private let rootURL: URL?

    public init(rootURL: URL? = nil) {
        let firstID = UUID()
        self.rootURL = rootURL
        _tabs = State(initialValue: [
            RuneWorkspaceTab(id: firstID, title: "Main", rootURL: rootURL, sessionID: nil),
        ])
        _selectedTabID = State(initialValue: firstID)
    }

    public var body: some View {
        VStack(spacing: 0) {
            tabBar
            if let selectedTab = tabs.first(where: { $0.id == selectedTabID }) {
                RuneTerminalView(rootURL: selectedTab.rootURL, sessionID: selectedTab.sessionID)
                    .id(selectedTab.id)
            } else {
                Text("Rune session unavailable")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
    }

    private var tabBar: some View {
        HStack(spacing: 8) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    ForEach(tabs) { tab in
                        HStack(spacing: 4) {
                            Button(tab.title) {
                                selectedTabID = tab.id
                            }
                            .buttonStyle(.plain)
                            .font(.system(size: 12, design: .monospaced))
                            .lineLimit(1)
                            if tabs.count > 1 {
                                Button {
                                    close(tab)
                                } label: {
                                    Image(systemName: "xmark")
                                        .font(.system(size: 9, weight: .bold))
                                }
                                .buttonStyle(.plain)
                                .accessibilityLabel("Close \(tab.title)")
                            }
                        }
                        .padding(.horizontal, 10)
                        .padding(.vertical, 7)
                        .background(tab.id == selectedTabID ? Color.accentColor.opacity(0.18) : Color.clear)
                        .clipShape(Capsule())
                    }
                }
            }
            Button {
                addTab()
            } label: {
                Image(systemName: "plus")
                    .font(.system(size: 13, weight: .bold))
            }
            .buttonStyle(.plain)
            .accessibilityLabel("New Rune session")
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(.ultraThinMaterial)
    }

    private func addTab() {
        let index = tabs.count + 1
        let tab = RuneWorkspaceTab(
            id: UUID(),
            title: "Session \(index)",
            rootURL: rootURL,
            sessionID: UUID().uuidString.lowercased()
        )
        tabs.append(tab)
        selectedTabID = tab.id
    }

    private func close(_ tab: RuneWorkspaceTab) {
        guard tabs.count > 1, let index = tabs.firstIndex(of: tab) else { return }
        tabs.remove(at: index)
        if selectedTabID == tab.id {
            selectedTabID = tabs[min(index, tabs.count - 1)].id
        }
    }
}
