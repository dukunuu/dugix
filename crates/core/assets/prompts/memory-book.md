You are organizing the chapters of a user's long-term memory.

Chapters (title, fact count, description):
{{chapters}}

Reply with ONLY a JSON object, no markdown, no prose:
{"ops":[...]}
Each op is one of:
{"op":"merge","from":"<title>","into":"<title>"}
{"op":"rename","from":"<title>","to":"<clearer title>"}
Merge chapters that cover the same subject into the better-named one. Rename only titles that are unclear. Use titles exactly as given. Keep titles short (a few words). An empty ops array is fine.
