// C++ Syntax Test
#include <concepts>
#include <future>
#include <string>

enum class Status { Draft, Published };

struct DocumentItem {
  int id;
  std::string title;
  const bool is_saved;
};



template <typename T>
concept IsDocument = requires(T doc) {
  { doc.id } -> std::convertible_to<int>;
  { doc.title } -> std::convertible_to<std::string>;
  { doc.is_saved } -> std::convertible_to<bool>;
};

template <IsDocument T> class EditorSession {
public:
  T doc;
  explicit EditorSession(T document) : doc(std::move(document)) {}

  std::future<bool> persist() {
    return std::async(std::launch::async,
                      [this]() { return this->doc.is_saved; });
  }
};

const EditorSession<DocumentItem> active{DocumentItem{101, "main.rs", true}};
