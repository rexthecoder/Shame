# Google Colab and Shame

**Short answer:** Shame doesn't connect to Colab runtimes, and there's no
legal, reliable way for a third-party editor to do it today. It does
connect to any **Jupyter server by URL**: a local `jupyter server` it starts
itself, JupyterHub, Kaggle, Vertex AI Workbench, RunPod, or any machine you
run `jupyter server` on. Pasting a Colab link into "Connect URL…" says so
instead of failing with a confusing error.

## How Google's VS Code extension connects (Nov 2026)

From the extension's source, [googlecolab/colab-vscode] (Apache-2.0):

1. **Google sign-in.** The extension runs an OAuth 2.0 flow (loopback
   redirect on desktop, PKCE) with the scopes `profile`, `email` and
   `https://www.googleapis.com/auth/colaboratory` (plus `drive.file` for
   Drive). It uses an OAuth client that Google registered for the
   extension; `.env.template` leaves `COLAB_EXTENSION_CLIENT_ID` empty, and
   contributors have to create their own client.
2. **Colab backend API.** With that token it calls Colab's own API (a v1
   client plus a generated `ColaboratoryApi` v2) to list and create
   runtimes ("assignments"), choose an accelerator, and read usage.
3. **Runtime connection.** For each runtime the API returns a
   `ConnectionInfo { url, endpoint, token, expireTime }`. The extension
   then talks to the Jupyter server on the runtime over HTTPS (no SSH),
   sending the short-lived token in an `X-Colab-Runtime-Proxy-Token`
   header, and refreshes it about 5 minutes before it expires
   (`connection-refresher.ts`).

## Why Shame doesn't do the same

- **The API isn't public.** The `colaboratory` scope and the runtime API
  aren't a documented developer API. An app needs its own Google OAuth
  client approved for that scope, and Google offers no way to get one for
  Colab.
- **Borrowing the extension's client is impersonation.** Using Google's
  client ID would present Shame as Google's extension, which breaks
  Google's OAuth policies and the
  [Colab Terms of Service](https://research.google.com/colaboratory/tos_v6.html).
  An account can be suspended for it.
- **It wouldn't stay working.** The API is internal and versioned by Google
  (v1 and v2 already coexist), so it can change under us at any time.
- **Tunnelling out of a Colab notebook** (starting `jupyter server` in a
  cell and exposing it with ngrok or cloudflared) works technically. But
  Colab's free tier lists remote access to the runtime as a disallowed
  activity, so Shame won't suggest it.

## What to use instead

- **Your own GPU or Apple Silicon:** open the notebook. Shame starts
  `jupyter server` from the project's environment and runs the kernel
  locally (Apple GPU through PyTorch MPS; see the AI panel).
- **Hosted GPUs:** Kaggle, Vertex AI Workbench, RunPod, Lambda, Paperspace
  or a JupyterHub all give you a Jupyter server URL with a `?token=`.
  Paste it into the notebook toolbar's **Connect URL…**. Kernels then run
  there, and "Use local" switches back.
- **Colab itself:** open the notebook in Colab's website, or in VS Code
  with Google's extension.

If Google publishes a Colab runtime API with OAuth clients for third
parties, it would fit Shame's existing design: one more `ServerInfo`
source whose URL and token come from that API, with the token refreshed
before `expireTime`.

[googlecolab/colab-vscode]: https://github.com/googlecolab/colab-vscode
