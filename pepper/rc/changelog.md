# changelog

## 0.23.0
- changed default clipboard linux interface to `xclip` instead of `xsel`
- fix crash when `lsp-references` would not load the context buffer

## 0.22.0
- added quit instruction to the start screen
- added '%t' to patterns to match a tab ('\t')
- fix bad handling of BSD's resize signal on kqueue

## 0.21.0
- prevent deadlocks by writing asynchronously to clients from server
- fix possible crash when listing lsp item references when there's a usage near the end of the buffer
- added instructions on how to package the web version of the editor
- added error to `lsp-stop` and `lsp-stop-all` when there is no lsp server running

## 0.20.0
- use builtin word database for completions when plugins provide no suggestions
- prevent closing all clients when the terminal from which the server was spawned is closed
- fix debugging crash when sometimes dropping a client connection

## 0.19.3
- added changelog! you can access it through `:help changelog<enter>`
- added error number to unix platform panics
- fix event loop on bsd
- fix idle events not triggering on unix
- fix buffer history undo crash when you undo after a "insert, delete then insert" single action
- fix messy multiple autocomplete on the same line
- fix crash on macos since there kqueue can't poll /dev/tty

## 0.19.2 and older
There was no official changelog before.
However, up to this point, we were implementing all features related to the editor's vision.
Then fixing bugs and stabilizing the code base.