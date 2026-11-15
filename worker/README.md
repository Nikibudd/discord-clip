# Soundboard service

A small Cloudflare Worker that lets discord-clip upload clips to Discord soundboards (#76). Each friend group hosts its own copy with its own bot, on Cloudflare's free plan.

The bot token lives only in the Worker. The app only knows the Worker's URL and, optionally, an upload key. Keep both private: anyone who has them can upload sounds to your servers.

## Setup

1. **Discord bot:** in the [Developer Portal](https://discord.com/developers/applications), create an application. On its **Bot** tab:
   - Click **Reset Token** and copy the token.
   - Turn **Public Bot** on only if other people should be able to add the bot to servers they manage.
   - No privileged intents are needed.
2. **Cloudflare:** create a free account. Under **Workers & Pages**, create a Worker from the **Hello World** template. Pick a hard-to-guess name, since it becomes part of the URL.
3. **Secrets:** in the Worker, open **Settings → Variables and Secrets** and add:
   - `DISCORD_BOT_TOKEN` (type Secret): the token from step 1.
   - `UPLOAD_KEY` (type Secret): optional, any random text.
4. **Deploy:** install [Node.js](https://nodejs.org), then in this folder:
   ```sh
   cp wrangler.example.jsonc wrangler.jsonc   # then set "name" to your Worker's name
   npx wrangler login
   npx wrangler deploy
   ```
   Without Node, paste `src/index.js` into the Worker's code editor in the dashboard and click **Deploy**.
5. **Check:** open the Worker's URL. It should answer `{"ok":true,"bot":{...}}`. If you set an upload key, it answers 401 instead, which also shows it's running.

## API

Every request sends `Authorization: Key <UPLOAD_KEY>` when a key is set.

`user` is the Discord user id of the person using the app. The Worker only shows and uploads to servers that both the bot and that user are in. The id isn't verified (there's no Discord sign-in), so the URL and the key are what actually keep strangers out.

| Request | Answer |
| --- | --- |
| `GET /` | `{ ok, bot: { id, name } }` |
| `GET /servers` | `{ servers: [{ id, name, icon, can_upload }] }`: all the bot's servers, in one Discord call. `can_upload` is false when the bot lacks **Create Expressions** there. The app hides servers the user isn't in by asking for each one's soundboard. |
| `GET /servers/<server id>?user=<id>` | `{ id, name, icon, tier, used, total, sounds: [{ id, name, emoji, available }] }`. `total` is `null` when Discord gave the server extra slots it doesn't report. A user who isn't in the server gets `403 { error, not_member: true }`. |
| `POST /invites` with `{ "links": "<pasted text>" }` | `{ servers: [{ id, name, icon, has_bot, install_url }], invalid: [codes] }`: at most 25 invite links at once. |
| `POST /upload` with `{ user, guild, name, sound, emoji?, volume? }` | `201 { id, name }`. `sound` is `data:audio/mpeg;base64,…` (or `audio/ogg`), at most 512 KB and 5.2 s; `name` is 2–32 characters; `emoji` is a Unicode emoji or a custom emoji id; `volume` is 0–1. |

Errors come back as `{ error }` with a matching status code. When Discord refused, the answer also carries Discord's own reply as `discord`. The server's audit log names the uploader's user id.

The bot is added with only the **Create Expressions** permission, so it can add sounds and change or delete its own, but not anyone else's.
