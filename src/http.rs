//! HTTPS via WinHTTP (uses the system's TLS, proxy settings and certificates).
//!
//! `get` streams a download (updates, avatars); `request` sends a body and
//! headers and returns the whole answer whatever its status (the soundboard
//! service, #76).

use std::ffi::c_void;

use anyhow::{bail, Context, Result};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::*;

use crate::update::CURRENT;

struct Handle(*mut c_void);

impl Handle {
    fn new(h: *mut c_void, what: &str) -> Result<Self> {
        if h.is_null() {
            Err(windows::core::Error::from_win32()).with_context(|| what.to_string())
        } else {
            Ok(Handle(h))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// An open request with its answer's status; the handles close on drop
/// (request first, then connection, then session).
struct Open {
    request: Handle,
    _connect: Handle,
    _session: Handle,
    status: u32,
}

impl Open {
    /// Reads the body in chunks.
    fn read(&self, mut on_chunk: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut n: u32 = 0;
            unsafe { WinHttpReadData(self.request.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut n) }
                .context("downloading")?;
            if n == 0 {
                return Ok(());
            }
            on_chunk(&buf[..n as usize])?;
        }
    }
}

fn open(method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> Result<Open> {
    let rest = url.strip_prefix("https://").context("URLs must use https")?;
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    if host.is_empty() || host.contains(['@', ':', ' ']) {
        bail!("not a valid address: {url}");
    }
    unsafe {
        let agent = HSTRING::from(format!("discord-clip/{CURRENT}"));
        let session = Handle::new(
            WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0),
            "starting WinHTTP",
        )?;
        WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 30_000)?;
        let connect = Handle::new(WinHttpConnect(session.0, &HSTRING::from(host), 443, 0), "connecting")?;
        let request = Handle::new(
            WinHttpOpenRequest(
                connect.0,
                &HSTRING::from(method),
                &HSTRING::from(path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ),
            "creating request",
        )?;
        let header_text: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
        let header_wide: Vec<u16> = header_text.encode_utf16().collect();
        let body = body.unwrap_or(&[]);
        let len = u32::try_from(body.len()).context("request too large")?;
        WinHttpSendRequest(
            request.0,
            (!header_wide.is_empty()).then_some(&header_wide[..]),
            (!body.is_empty()).then_some(body.as_ptr() as *const c_void),
            len,
            len,
            0,
        )
        .with_context(|| format!("connecting to {host}"))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).with_context(|| format!("waiting for {host}"))?;

        let mut status: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )?;
        Ok(Open { request, _connect: connect, _session: session, status })
    }
}

/// GETs a file from GitHub (following redirects), passing the body to
/// `on_chunk`. Anything but 200 is an error.
pub(crate) fn get(url: &str, on_chunk: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
    let open = open("GET", url, &[], None)?;
    match open.status {
        200 => open.read(on_chunk),
        404 => bail!("no release found on GitHub (HTTP 404)"),
        code => bail!("GitHub answered HTTP {code}"),
    }
}

/// An answer: its status and the whole body.
pub(crate) struct Response {
    pub status: u32,
    pub body: Vec<u8>,
}

/// Sends a request and returns the answer, whatever its status. Answers
/// larger than `max_body` bytes are an error.
pub(crate) fn request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    max_body: usize,
) -> Result<Response> {
    let open = open(method, url, headers, body)?;
    let mut out = Vec::new();
    open.read(|chunk| {
        if out.len() + chunk.len() > max_body {
            bail!("the answer is too large");
        }
        out.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(Response { status: open.status, body: out })
}
