// Rust Syntax Test
pub enum Status {
    Draft,
    Published,
}

pub trait DocumentItem {
    fn id(&self) -> u64;
    fn title(&self) -> &str;
    fn is_saved(&self) -> bool;
}

#[derive(Debug, Clone)]
pub struct Document {
    pub id: u64,
    pub title: String,
    pub is_saved: bool,
}

impl DocumentItem for Document {
    fn id(&self) -> u64 {
        self.id
    }
    fn title(&self) -> &str {
        &self.title
    }
    fn is_saved(&self) -> bool {
        self.is_saved
    }
}

pub struct EditorSession<T: DocumentItem> {
    pub doc: T,
}

impl<T: DocumentItem> EditorSession<T> {
    pub fn new(doc: T) -> Self {
        Self { doc }
    }
    pub async fn persist(&self) -> bool {
        self.doc.is_saved()
    }
}

pub fn active() -> EditorSession<Document> {
    EditorSession::new(Document {
        id: 101,
        title: String::from("main.rs"),
        is_saved: true,
    })
}
