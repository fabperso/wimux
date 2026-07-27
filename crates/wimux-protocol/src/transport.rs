// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fabrice Andy
//! Transport IPC : Named Pipe Windows duplex, en I/O **overlapped**.
//!
//! Point crucial : un handle ouvert en I/O synchrone sérialise les opérations.
//! Si un thread est bloqué dans un `ReadFile` (en attente d'une frame), un
//! `WriteFile` concurrent sur le même handle **attend la fin de la lecture** —
//! ce qui provoque un interblocage dans un multiplexeur où le serveur pousse des
//! frames pendant que le client envoie des frappes. On ouvre donc les pipes avec
//! `FILE_FLAG_OVERLAPPED` : chaque opération est indépendante, et lecture et
//! écriture peuvent réellement se dérouler en parallèle sur le même handle.
//!
//! `Read`/`Write` sont implémentés pour `&PipeConn`, afin de partager un
//! `Arc<PipeConn>` entre un thread lecteur et un thread écrivain.

use std::io::{self, Read, Write};
use std::sync::Arc;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    ReadFile, WriteFile,
};
use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::core::PCWSTR;

use crate::PIPE_PREFIX;

const PIPE_BUFFER: u32 = 64 * 1024;

/// Nom de pipe propre à l'utilisateur courant : `\\.\pipe\wimux-<user>`.
pub fn user_pipe_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".to_string());
    format!("{PIPE_PREFIX}-{user}")
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_os_code() -> u32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32
}

/// Une extrémité de Named Pipe (duplex, mode octet, overlapped).
pub struct PipeConn {
    handle: HANDLE,
}

unsafe impl Send for PipeConn {}
unsafe impl Sync for PipeConn {}

impl PipeConn {
    fn read_impl(&self, buf: &mut [u8]) -> io::Result<usize> {
        unsafe {
            overlapped_op(self.handle, |ov| {
                ReadFile(self.handle, Some(buf), None, Some(ov))
            })
        }
    }

    fn write_impl(&self, buf: &[u8]) -> io::Result<usize> {
        unsafe {
            overlapped_op(self.handle, |ov| {
                WriteFile(self.handle, Some(buf), None, Some(ov))
            })
        }
    }
}

/// Exécute une opération overlapped (lecture ou écriture) et attend sa fin.
/// Renvoie le nombre d'octets transférés (0 = fin de flux propre).
unsafe fn overlapped_op<F>(handle: HANDLE, start: F) -> io::Result<usize>
where
    F: FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>,
{
    unsafe {
        let event = CreateEventW(None, true, false, PCWSTR::null())
            .map_err(|_| io::Error::from_raw_os_error(last_os_code() as i32))?;
        let mut ov = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };

        let started = start(&mut ov);
        let result = match started {
            Ok(()) => finish(handle, &ov),
            Err(_) => {
                let code = last_os_code();
                if code == ERROR_IO_PENDING.0 {
                    WaitForSingleObject(event, INFINITE);
                    finish(handle, &ov)
                } else {
                    eof_or_err(code)
                }
            }
        };
        let _ = CloseHandle(event);
        result
    }
}

unsafe fn finish(handle: HANDLE, ov: &OVERLAPPED) -> io::Result<usize> {
    unsafe {
        let mut transferred = 0u32;
        match GetOverlappedResult(handle, ov, &mut transferred, false) {
            Ok(()) => Ok(transferred as usize),
            Err(_) => eof_or_err(last_os_code()),
        }
    }
}

fn eof_or_err(code: u32) -> io::Result<usize> {
    if code == ERROR_BROKEN_PIPE.0 || code == ERROR_PIPE_NOT_CONNECTED.0 {
        Ok(0)
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

impl Drop for PipeConn {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

impl Read for &PipeConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_impl(buf)
    }
}

impl Write for &PipeConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_impl(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for PipeConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_impl(buf)
    }
}

impl Write for PipeConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_impl(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Se connecte à un serveur existant. Renvoie `NotFound` si aucun serveur
/// n'écoute (le pipe n'existe pas encore).
pub fn connect(name: &str) -> io::Result<PipeConn> {
    let wide = to_wide(name);
    let access = GENERIC_READ.0 | GENERIC_WRITE.0;
    loop {
        let res = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                access,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        };
        match res {
            Ok(handle) if !handle.is_invalid() => return Ok(PipeConn { handle }),
            _ => {
                let code = last_os_code();
                if code == ERROR_PIPE_BUSY.0 {
                    let _ = unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), 2000) };
                    continue;
                }
                if code == ERROR_FILE_NOT_FOUND.0 {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "aucun serveur wimux à l'écoute",
                    ));
                }
                return Err(io::Error::from_raw_os_error(code as i32));
            }
        }
    }
}

/// Écouteur côté serveur. Chaque appel à [`PipeListener::accept`] crée une
/// nouvelle instance de pipe et bloque jusqu'à la connexion d'un client.
pub struct PipeListener {
    wide_name: Vec<u16>,
}

impl PipeListener {
    pub fn bind(name: &str) -> Self {
        PipeListener {
            wide_name: to_wide(name),
        }
    }

    pub fn accept(&self) -> io::Result<PipeConn> {
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(self.wide_name.as_ptr()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                None,
            )
        };
        if handle.is_invalid() {
            return Err(io::Error::from_raw_os_error(last_os_code() as i32));
        }

        // ConnectNamedPipe sur un pipe overlapped se termine de façon asynchrone.
        let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
            .map_err(|_| io::Error::from_raw_os_error(last_os_code() as i32))?;
        let mut ov = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };

        let connect_res = unsafe { ConnectNamedPipe(handle, Some(&mut ov)) };
        let outcome = match connect_res {
            Ok(()) => Ok(()),
            Err(_) => {
                let code = last_os_code();
                if code == ERROR_IO_PENDING.0 {
                    unsafe { WaitForSingleObject(event, INFINITE) };
                    let mut transferred = 0u32;
                    unsafe { GetOverlappedResult(handle, &ov, &mut transferred, false) }
                        .map_err(|_| io::Error::from_raw_os_error(last_os_code() as i32))
                } else if code == ERROR_PIPE_CONNECTED.0 {
                    Ok(())
                } else {
                    Err(io::Error::from_raw_os_error(code as i32))
                }
            }
        };

        unsafe {
            let _ = CloseHandle(event);
        }

        match outcome {
            Ok(()) => Ok(PipeConn { handle }),
            Err(e) => {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                Err(e)
            }
        }
    }
}

/// Facilite le partage d'une connexion entre threads lecteur/écrivain.
pub fn shared(conn: PipeConn) -> Arc<PipeConn> {
    Arc::new(conn)
}
