import SwiftUI

struct ChatView: View {
    @ObservedObject var model: JarvisAppModel
    @State private var draft = ""

    var body: some View {
        NavigationStack {
            Group {
                if model.connectionState != .reachable || !model.isAuthenticated {
                    EnrollmentView(model: model)
                } else {
                    conversation
                }
            }
            .navigationTitle(model.currentConversationTitle)
            .toolbar {
                if model.isAuthenticated {
                    ToolbarItem(placement: .topBarTrailing) {
                        Menu {
                            Button("New conversation", systemImage: "square.and.pencil") { model.newConversation() }
                            ForEach(model.conversations) { item in
                                Button(item.title) { Task { await model.openConversation(item.id) } }
                            }
                        } label: { Image(systemName: "ellipsis.circle") }
                    }
                }
            }
        }
    }

    private var conversation: some View {
        VStack(spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 12) {
                        ForEach(model.messages) { message in
                            MessageBubble(message: message).id(message.id)
                        }
                        if model.isSending { ProgressView().padding() }
                    }
                    .padding()
                }
                .onChange(of: model.messages.count) { _, _ in
                    if let last = model.messages.last { proxy.scrollTo(last.id, anchor: .bottom) }
                }
            }
            Divider()
            HStack(alignment: .bottom, spacing: 10) {
                TextField("Message Jarvis", text: $draft, axis: .vertical)
                    .textFieldStyle(.roundedBorder)
                    .lineLimit(1...6)
                    .submitLabel(.send)
                    .onSubmit { send() }
                Button(action: send) { Image(systemName: "arrow.up.circle.fill").font(.title2) }
                    .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.isSending)
                    .accessibilityLabel("Send message")
            }
            .padding()
            .background(.bar)
        }
    }

    private func send() {
        let message = draft
        draft = ""
        Task { await model.send(message) }
    }
}

private struct MessageBubble: View {
    let message: ConversationMessage

    var body: some View {
        HStack {
            if !message.isAssistant { Spacer(minLength: 48) }
            Text(message.content)
                .textSelection(.enabled)
                .padding(12)
                .background(message.isAssistant ? Color.secondary.opacity(0.13) : Color.accentColor.opacity(0.18))
                .clipShape(RoundedRectangle(cornerRadius: 16))
            if message.isAssistant { Spacer(minLength: 48) }
        }
    }
}
