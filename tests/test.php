<?php
// PHP Syntax Test

enum Status: string {
    case Draft = 'draft';
    case Published = 'published';
}

interface DocumentItem {
    public function getId(): int;
    public function getTitle(): string;
    public function isSaved(): bool;
}

readonly class Document implements DocumentItem {
    public function __construct(
        public int $id,
        public string $title,
        public bool $isSaved
    ) {}

    public function getId(): int { return $this->id; }
    public function getTitle(): string { return $this->title; }
    public function isSaved(): bool { return $this->isSaved; }
}

/**
 * @template T of DocumentItem
 */
class EditorSession {
    public function __construct(public DocumentItem $doc) {}

    public function persist(): bool {
        return $this->doc->isSaved();
    }
}

$active = new EditorSession(new Document(101, 'main.rs', true));

