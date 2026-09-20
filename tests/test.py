# Python Syntax Test
class TextBuffer:
    def __init__(self, filename: str, max_lines: int = 1000):
        self.filename = filename
        self.max_lines = max_lines
        self.is_modified = False

    def append_line(self, content: str) -> bool:
        if not content or self.is_modified:
            return False
        print(f"Saving to {self.filename}")
        return True
