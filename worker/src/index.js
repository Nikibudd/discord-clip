// Soundboard service for discord-clip (issue #76).
//
// Holds the group's bot token (secret DISCORD_BOT_TOKEN) and uploads clips to
// soundboards for the app. An optional secret UPLOAD_KEY must then be sent by
// the app as `Authorization: Key <key>`.
//
// Endpoints (see README.md for the shapes):
//   GET  /                         checks the token works
//   GET  /servers?user=<id>        servers shared by the bot and that user
//   GET  /servers/<id>?user=<id>   one server's soundboard: slots and sounds
//   POST /invites                  invite links -> servers missing the bot
//   POST /upload                   adds a sound
//
// The free plan allows 50 Discord calls per request, so the server list only
// checks membership; slots are fetched per server.

const API = "https://discord.com/api/v10";

// Permission bits (BigInt: they don't fit in 32 bits).
const ADMINISTRATOR = 1n << 3n;
const CREATE_GUILD_EXPRESSIONS = 1n << 43n;
const PERMISSIONS = String(CREATE_GUILD_EXPRESSIONS);

// Soundboard slots per boost tier. Discord doesn't report the limit.
const SLOTS = [8, 24, 36, 48];

const MAX_SOUND_BYTES = 512 * 1024;
const SNOWFLAKE = /^\d{15,21}$/;
const INVITE = /(?:discord\.gg|discord(?:app)?\.com\/invite)\/([A-Za-z0-9-]{2,32})/g;
const MAX_INVITES = 25;

export default {
  async fetch(request, env) {
    try {
      if (!env.DISCORD_BOT_TOKEN) {
        return json({ error: "DISCORD_BOT_TOKEN is not set on this Worker." }, 500);
      }
      if (!keyOk(request, env)) {
        return json({ error: "Wrong or missing upload key." }, 401);
      }

      const url = new URL(request.url);
      const path = url.pathname.replace(/\/+$/, "") || "/";
      const one = path.match(/^\/servers\/(\d{15,21})$/);
      if (request.method === "GET" && path === "/") return await status(env);
      if (request.method === "GET" && path === "/servers") {
        return await servers(env, userParam(url));
      }
      if (request.method === "GET" && one) {
        return await server(env, one[1], userParam(url));
      }
      if (request.method === "POST" && path === "/invites") {
        return await invites(env, await body(request));
      }
      if (request.method === "POST" && path === "/upload") {
        return await upload(env, await body(request));
      }
      return json({ error: "Not found." }, 404);
    } catch (e) {
      if (e instanceof BadRequest) return json({ error: e.message }, e.status);
      if (e instanceof DiscordError) {
        return json({ error: e.message, discord: e.body }, e.status);
      }
      return json({ error: String(e) }, 500);
    }
  },
};

async function status(env) {
  const me = await discord(env, "GET", "/users/@me");
  return json({ ok: true, bot: { id: me.id, name: me.username } });
}

// Servers the bot and the user are both in.
async function servers(env, user) {
  const guilds = await botGuilds(env);
  const shared = [];
  for (const g of guilds) {
    if (await isMember(env, g.id, user)) shared.push(g);
  }
  return json({
    servers: shared.map((g) => ({
      id: g.id,
      name: g.name,
      icon: iconUrl(g),
      can_upload: canUpload(g.permissions),
    })),
  });
}

// One server's soundboard.
async function server(env, guildId, user) {
  await requireMember(env, guildId, user);
  const g = await discord(env, "GET", `/guilds/${guildId}`);
  const list = await discord(env, "GET", `/guilds/${guildId}/soundboard-sounds`);
  const sounds = list.items || [];
  const tier = g.premium_tier || 0;
  // Servers with this feature get extra slots by an amount Discord doesn't say.
  const total = (g.features || []).includes("MORE_SOUNDBOARD") ? null : SLOTS[tier] ?? null;
  return json({
    id: g.id,
    name: g.name,
    icon: iconUrl(g),
    tier,
    used: sounds.length,
    total,
    sounds: sounds.map((s) => ({
      id: s.sound_id,
      name: s.name,
      emoji: s.emoji_name || s.emoji_id || null,
      available: s.available !== false,
    })),
  });
}

// Pasted text with invite links -> the servers they lead to, and an install
// link for each one the bot isn't in yet.
async function invites(env, input) {
  const text = Array.isArray(input.links) ? input.links.join("\n") : String(input.links ?? "");
  const codes = [...new Set([...text.matchAll(INVITE)].map((m) => m[1]))];
  if (codes.length === 0) throw new BadRequest("No invite links found.");
  if (codes.length > MAX_INVITES) {
    throw new BadRequest(`At most ${MAX_INVITES} invite links at once.`);
  }
  const app = await discord(env, "GET", "/oauth2/applications/@me");
  const have = new Set((await botGuilds(env)).map((g) => g.id));
  const found = new Map();
  const invalid = [];
  for (const code of codes) {
    let inv;
    try {
      inv = await discord(env, "GET", `/invites/${encodeURIComponent(code)}`);
    } catch (e) {
      if (e instanceof DiscordError && e.status === 404) {
        invalid.push(code);
        continue;
      }
      throw e;
    }
    const g = inv.guild;
    if (!g || found.has(g.id)) continue;
    found.set(g.id, {
      id: g.id,
      name: g.name,
      icon: iconUrl(g),
      has_bot: have.has(g.id),
      install_url: have.has(g.id) ? null : installUrl(app.id, g.id),
    });
  }
  return json({ servers: [...found.values()], invalid });
}

// Adds a sound. Body: { user, guild, name, sound: "data:audio/mpeg;base64,...",
// emoji?, volume? }
async function upload(env, input) {
  const user = String(input.user ?? "");
  const guild = String(input.guild ?? "");
  if (!SNOWFLAKE.test(user)) throw new BadRequest("Missing or invalid user id.");
  if (!SNOWFLAKE.test(guild)) throw new BadRequest("Missing or invalid server id.");

  const name = String(input.name ?? "").trim();
  if ([...name].length < 2 || [...name].length > 32) {
    throw new BadRequest("The name must be 2 to 32 characters.");
  }

  const sound = String(input.sound ?? "");
  const m = sound.match(/^data:audio\/(mpeg|mp3|ogg);base64,([A-Za-z0-9+/]+=*)$/);
  if (!m) throw new BadRequest("The sound must be an MP3 or Ogg data URI.");
  const bytes = Math.floor((m[2].length * 3) / 4) - (m[2].match(/=*$/)[0].length);
  if (bytes > MAX_SOUND_BYTES) {
    throw new BadRequest("The sound is larger than Discord's 512 KB limit.");
  }

  const payload = { name, sound: `data:audio/${m[1] === "ogg" ? "ogg" : "mpeg"};base64,${m[2]}` };
  if (input.volume !== undefined && input.volume !== null) {
    const v = Number(input.volume);
    if (!(v >= 0 && v <= 1)) throw new BadRequest("Volume must be between 0 and 1.");
    payload.volume = v;
  }
  const emoji = String(input.emoji ?? "").trim();
  if (emoji) {
    if (SNOWFLAKE.test(emoji)) payload.emoji_id = emoji;
    else payload.emoji_name = emoji;
  }

  await requireMember(env, guild, user);
  const s = await discord(
    env,
    "POST",
    `/guilds/${guild}/soundboard-sounds`,
    payload,
    `Uploaded by user ${user} with discord-clip`,
  );
  return json({ id: s.sound_id, name: s.name }, 201);
}

// --- Discord lookups -------------------------------------------------------

async function botGuilds(env) {
  const all = [];
  let after = "0";
  for (;;) {
    const page = await discord(env, "GET", `/users/@me/guilds?limit=200&after=${after}`);
    all.push(...page);
    if (page.length < 200) return all;
    after = page[page.length - 1].id;
  }
}

async function isMember(env, guildId, user) {
  try {
    await discord(env, "GET", `/guilds/${guildId}/members/${user}`);
    return true;
  } catch (e) {
    // 10007 Unknown Member, 10013 Unknown User.
    if (e instanceof DiscordError && e.status === 404) return false;
    throw e;
  }
}

async function requireMember(env, guildId, user) {
  if (!(await isMember(env, guildId, user))) {
    throw new BadRequest("You're not in that server, or the bot isn't.", 403);
  }
}

function canUpload(permissions) {
  try {
    const p = BigInt(permissions ?? "0");
    return (p & (ADMINISTRATOR | CREATE_GUILD_EXPRESSIONS)) !== 0n;
  } catch {
    return false;
  }
}

function iconUrl(g) {
  if (!g.icon) return null;
  const ext = g.icon.startsWith("a_") ? "gif" : "png";
  return `https://cdn.discordapp.com/icons/${g.id}/${g.icon}.${ext}?size=64`;
}

function installUrl(appId, guildId) {
  const q = new URLSearchParams({
    client_id: appId,
    scope: "bot",
    permissions: PERMISSIONS,
    guild_id: guildId,
    disable_guild_select: "true",
    integration_type: "0",
  });
  return `https://discord.com/oauth2/authorize?${q}`;
}

// --- helpers ---------------------------------------------------------------

class BadRequest extends Error {
  constructor(message, status = 400) {
    super(message);
    this.status = status;
  }
}

function userParam(url) {
  const user = url.searchParams.get("user") || "";
  if (!SNOWFLAKE.test(user)) throw new BadRequest("Missing or invalid user id.");
  return user;
}

async function body(request) {
  try {
    const v = await request.json();
    if (v && typeof v === "object") return v;
  } catch {}
  throw new BadRequest("The request body must be a JSON object.");
}

function keyOk(request, env) {
  if (!env.UPLOAD_KEY) return true;
  const got = request.headers.get("Authorization") || "";
  return timingSafeEqual(got, `Key ${env.UPLOAD_KEY}`);
}

function timingSafeEqual(a, b) {
  const enc = new TextEncoder();
  const x = enc.encode(a);
  const y = enc.encode(b);
  let diff = x.length ^ y.length;
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    diff |= (x[i] ?? 0) ^ (y[i] ?? 0);
  }
  return diff === 0;
}

class DiscordError extends Error {
  constructor(status, body) {
    super((body && body.message) || `Discord answered ${status}.`);
    this.status = status;
    this.body = body;
  }
}

// Calls the Discord API as the bot. Waits once on a 429 if Discord asks for a
// short pause; otherwise passes the error on.
async function discord(env, method, path, body, reason) {
  for (let attempt = 0; ; attempt++) {
    const headers = { Authorization: `Bot ${env.DISCORD_BOT_TOKEN}` };
    if (body !== undefined) headers["Content-Type"] = "application/json";
    if (reason) headers["X-Audit-Log-Reason"] = encodeURIComponent(reason);
    const res = await fetch(API + path, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (res.status === 204) return null;
    const data = await res.json().catch(() => null);
    if (res.ok) return data;
    const wait = Number(data?.retry_after ?? res.headers.get("Retry-After"));
    if (res.status === 429 && attempt === 0 && wait > 0 && wait <= 5) {
      await new Promise((r) => setTimeout(r, wait * 1000));
      continue;
    }
    throw new DiscordError(res.status, data);
  }
}

function json(value, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
