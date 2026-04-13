/**
 * Log bridge: forwards frontend console.log/warn/error/debug calls to Rust,
 * where they appear in the terminal with FE: prefixed targets.
 *
 * Batches entries for 100ms before sending in one IPC call.
 */

import { invoke } from '@tauri-apps/api/core'

interface LogEntry {
    level: string
    category: string
    message: string
}

let pending: LogEntry[] = []
let timer: ReturnType<typeof setTimeout> | null = null

function addEntry(level: string, category: string, message: string) {
    pending.push({ level, category, message })
    if (!timer) {
        timer = setTimeout(() => {
            void flush()
        }, 100)
    }
}

async function flush() {
    timer = null
    if (pending.length === 0) return
    const entries = pending
    pending = []
    try {
        await invoke('batch_fe_logs', { entries })
    } catch {
        // Backend not ready — silently drop
    }
}

/**
 * Override console.log/warn/error/debug to forward to Rust.
 * Keeps original console methods working (for browser devtools).
 * Category is extracted from the first arg if it matches [category] pattern.
 */
export function initLogBridge() {
    const origLog = console.log
    const origWarn = console.warn
    const origError = console.error
    const origDebug = console.debug

    function formatArgs(args: unknown[]): { category: string; message: string } {
        const text = args.map(String).join(' ')
        // Extract [category] prefix if present, e.g. "[viewer] something" -> category="viewer"
        const match = text.match(/^\[(\w+)\]\s*(.*)/)
        if (match) {
            return { category: match[1], message: match[2] }
        }
        return { category: 'app', message: text }
    }

    console.log = (...args: unknown[]) => {
        origLog(...args)
        const { category, message } = formatArgs(args)
        addEntry('debug', category, message)
    }
    console.warn = (...args: unknown[]) => {
        origWarn(...args)
        const { category, message } = formatArgs(args)
        addEntry('warn', category, message)
    }
    console.error = (...args: unknown[]) => {
        origError(...args)
        const { category, message } = formatArgs(args)
        addEntry('error', category, message)
    }
    console.debug = (...args: unknown[]) => {
        origDebug(...args)
        const { category, message } = formatArgs(args)
        addEntry('debug', category, message)
    }
}
