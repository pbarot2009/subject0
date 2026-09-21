// Java Syntax Test
import java.util.concurrent.CompletableFuture;

enum Status { DRAFT, PUBLISHED }

interface DocumentItem {
    int id();
    String title();
    boolean isSaved();
}

record Document(int id, String title, boolean isSaved) implements DocumentItem {}

class EditorSession<T extends DocumentItem> {
    public final T doc;
    public EditorSession(T doc) { this.doc = doc; }

    public CompletableFuture<Boolean> persist() {
        return CompletableFuture.completedFuture(doc.isSaved());
    }
}

public class Main {
    public static final EditorSession<Document> ACTIVE =
        new EditorSession<>(new Document(101, "main.rs", true));
}

