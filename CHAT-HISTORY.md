# Saved Agent conversations

The Agent panel saves conversations per signed-in account and project. Use **Chat history** to reopen a conversation or **New chat** to start with an empty context. The last selected conversation reopens when the project is opened again.

The archive contains both the visible transcript and the model context, including text tool results. Page images and freshly supplied project context are not retained in the model history. Requests interrupted before completion appear as interrupted when the app reopens. Failed or cancelled requests remain visible. Switching conversations is disabled while a request is active.

Archives are written atomically under the operating system's application data directory, in `Koharu/agent-chats`. On Linux, `XDG_DATA_HOME` controls the parent directory. Keep that directory on the same drive as projects and caches when using an external drive. Back up this directory to preserve conversations. Authentication credentials continue to use the existing secret store.

Existing chats lost before this feature was installed cannot be recovered: upstream held that history only in process memory. A malformed archive is reported and preserved rather than replaced. Saved conversations remain available if the model-list refresh is offline.

The fork's updater points to this repository's releases rather than upstream, so an upstream binary cannot silently replace this feature.
