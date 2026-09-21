// Swift Syntax Test
enum Status: String {
    case draft, published
}

protocol DocumentItem {
    var id: Int { get }
    var title: String { get }
    var isSaved: Bool { get }
}

struct Document: DocumentItem {
    let id: Int
    let title: String
    let isSaved: Bool
}

final class EditorSession<T: DocumentItem> {
    var doc: T
    init(doc: T) { self.doc = doc }

    func persist() async -> Bool {
        return doc.isSaved
    }
}

let active = EditorSession(doc: Document(id: 101, title: "main.rs", isSaved: true))

