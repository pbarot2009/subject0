// TypeScript Syntax Test
interface DocumentItem { id: number; title: string; readonly isSaved: boolean; }
type Status = "draft" | "published";

class EditorSession<T extends DocumentItem> {
  public doc: T;
  constructor(doc: T) { this.doc = doc; }
  async persist(): Promise<boolean> {
    return this.doc.isSaved;
  }
}

export const active = new EditorSession({ id: 101, title: "main.rs", isSaved: true });

