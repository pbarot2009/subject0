# Ruby Syntax Test
module Status
  DRAFT = :draft
  PUBLISHED = :published
end

DocumentItem = Data.define(:id, :title, :is_saved)

class EditorSession
  attr_accessor :doc

  def initialize(doc)
    raise TypeError, "Expected DocumentItem" unless doc.is_a?(DocumentItem)
    @doc = doc
  end

  def persist
    Thread.new { @doc.is_saved }.value
  end
end

ACTIVE = EditorSession.new(DocumentItem.new(id: 101, title: "main.rs", is_saved: true))

