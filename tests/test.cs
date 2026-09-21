// C# Syntax Test
using System.Threading.Tasks;

public enum Status { Draft, Published }

public interface IDocumentItem {
    int Id { get; }
    string Title { get; }
    bool IsSaved { get; }
}

public record Document(int Id, string Title, bool IsSaved) : IDocumentItem;

public class EditorSession<T> where T : IDocumentItem {
    public T Doc { get; set; }
    public EditorSession(T doc) => Doc = doc;

    public async Task<bool> PersistAsync() {
        return await Task.FromResult(Doc.IsSaved);
    }
}

public static class Program {
    public static readonly EditorSession<Document> Active =
        new(new Document(101, "main.rs", true));
}

