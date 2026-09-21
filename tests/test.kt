// Kotlin Syntax Test
enum class Status { DRAFT, PUBLISHED }

interface DocumentItem {
    val id: Int
    val title: String
    val isSaved: Boolean
}

data class Document(
    override val id: Int,
    override val title: String,
    override val isSaved: Boolean
) : DocumentItem

class EditorSession<T : DocumentItem>(val doc: T) {
    suspend fun persist(): Boolean = doc.isSaved
}

val active = EditorSession(Document(id = 101, title = "main.rs", isSaved = true))

