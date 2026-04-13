import { invoke } from '@tauri-apps/api/core'
import { convertFileSrc } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
export { convertFileSrc, listen }

// Response types
export interface StateResponse {
    filePath: string | null
    index: number
    total: number
    onboarding: boolean
}
export interface NavigateResponse {
    filePath: string | null
    index: number
    total: number
}
export interface OnboardingInfo {
    version: string
    handlerStatus: string
    notInApplications: boolean
}

// Commands
export const getState = () => invoke<StateResponse>('get_state')
export const navigate = (forward: boolean) => invoke<NavigateResponse>('navigate', { forward })
export const getAdjacentPaths = (count: number) => invoke<string[]>('get_adjacent_paths', { count })
export const toggleFullscreen = () => invoke<boolean>('toggle_fullscreen')
export const setFullscreen = (on: boolean) => invoke<boolean>('set_fullscreen', { on })
export const handleEscape = () => invoke<null>('handle_escape')
export const setAsDefaultViewer = () => invoke<string>('set_as_default_viewer')
export const getOnboardingInfo = () => invoke<OnboardingInfo>('get_onboarding_info')
export const reportZoomPan = (zoom: number, panX: number, panY: number, windowWidth: number, windowHeight: number) =>
    invoke<null>('report_zoom_pan', { zoom, panX, panY, windowWidth, windowHeight })
export const openFile = (path: string) => invoke<NavigateResponse>('open_file', { path })
