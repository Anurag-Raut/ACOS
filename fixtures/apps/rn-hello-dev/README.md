# rn-hello-dev — the Metro project the `dev_server` scenarios edit (METRO3)

The same app as `../rn-hello` laid out the way React Native's own template is:
`App.js` exports the component (a Fast Refresh boundary) and `index.js` registers
it. `scenarios/metro-reload*.yaml` point `dev_server: {op: start, project: ...}`
here; the executor **copies** this directory into run scratch, hard-links the host's
React Native `node_modules` next to it and edits only the copy. This source tree is
never edited or served.
