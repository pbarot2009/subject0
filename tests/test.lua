-- Lua Syntax Test

local Status = {
    DRAFT = "draft",
    PUBLISHED = "published",
}

---@class DocumentItem
---@field id integer
---@field title string
---@field isSaved boolean

---@class EditorSession
local EditorSession = {}
EditorSession.__index = EditorSession

function EditorSession.new(doc)
    local self = setmetatable({}, EditorSession)
    self.doc = doc
    return self
end

function EditorSession:persist()
    -- Asynchronous execution via coroutine
    local co = coroutine.create(function()
        return self.doc.isSaved
    end)
    local _, result = coroutine.resume(co)
    return result
end

local active = EditorSession.new({
    id = 101,
    title = "main.rs",
    isSaved = true,
})

return {
    Status = Status,
    EditorSession = EditorSession,
    active = active,
}

