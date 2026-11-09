You are tidying one chapter of a user's long-term memory.

Chapter: {{title}}
Current description: {{description}}
Facts (id, then text, oldest first):
{{facts}}

Reply with ONLY a JSON object, no markdown, no prose:
{"description":"<one line on what this chapter holds>","ops":[...]}
Each op is one of:
{"op":"delete","id":"<id>","why":"<short reason>"}
{"op":"update","id":"<id>","text":"<clearer replacement>"}
{"op":"merge","ids":["<id>","<id>"],"text":"<one fact replacing them>"}
{"op":"conflict","ids":["<id>","<id>"],"note":"<what the user must decide>"}
Merge duplicates and near-duplicates. Delete a fact only when a newer fact clearly replaces it or it is not durable (a one-off, a summary of a conversation). Rewrite vague facts to be short and specific. When two facts disagree and neither clearly replaces the other, report a conflict instead of choosing. Never invent facts. Use the ids exactly as given. An empty ops array is fine.
