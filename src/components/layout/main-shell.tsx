"use client"

import { useEffect } from "react"
import { useRouter, useSearchParams } from "next/navigation"
import { FolderNavTree } from "@/components/layout/folder-nav-tree"
import { FolderWorkspace } from "@/components/layout/folder-workspace"
import { useFolderNav } from "@/contexts/folder-nav-context"

export function MainShell() {
  const { activeFolderId, findFolder, activateFolder, loading, loadedFolders } =
    useFolderNav()
  const searchParams = useSearchParams()
  const router = useRouter()

  // Sync URL ↔ activeFolderId. URL wins on initial load (deep-linking).
  useEffect(() => {
    const param = searchParams.get("folder")
    if (!param) return
    const parsed = Number.parseInt(param, 10)
    if (!Number.isFinite(parsed)) return
    if (loading) return
    if (parsed === activeFolderId) return
    const exists = !!findFolder(parsed)
    if (exists) {
      activateFolder(parsed)
    }
  }, [searchParams, loading, findFolder, activateFolder, activeFolderId])

  // Keep URL in sync with activeFolderId changes from the nav tree.
  useEffect(() => {
    const current = searchParams.get("folder")
    const next = activeFolderId != null ? String(activeFolderId) : null
    if (current === next) return
    const params = new URLSearchParams(searchParams.toString())
    if (next) {
      params.set("folder", next)
    } else {
      params.delete("folder")
    }
    const qs = params.toString()
    router.replace(qs ? `/main?${qs}` : "/main")
  }, [activeFolderId, router, searchParams])

  // Ask xterm / Monaco / virtual scrollers inside the activated folder to
  // re-measure. They were rendered under `display:none` while inactive, so
  // their cached sizes may be stale.
  useEffect(() => {
    if (activeFolderId == null) return
    const id = requestAnimationFrame(() => {
      window.dispatchEvent(new Event("resize"))
    })
    return () => cancelAnimationFrame(id)
  }, [activeFolderId])

  // Single owner of document.title — avoids multiple keep-alive folder
  // instances fighting for it.
  useEffect(() => {
    const folder = activeFolderId != null ? findFolder(activeFolderId) : null
    document.title = folder ? `${folder.name} - codeg` : "codeg"
  }, [activeFolderId, findFolder])

  return (
    <div className="flex h-screen w-screen overflow-hidden">
      <aside className="w-[240px] flex-shrink-0">
        <FolderNavTree />
      </aside>
      <main className="flex-1 min-w-0 overflow-hidden">
        <FolderPane
          activeFolderId={activeFolderId}
          loadedFolders={loadedFolders}
        />
      </main>
    </div>
  )
}

interface FolderPaneProps {
  activeFolderId: number | null
  loadedFolders: ReturnType<typeof useFolderNav>["loadedFolders"]
}

function FolderPane({ activeFolderId, loadedFolders }: FolderPaneProps) {
  const showEmpty =
    activeFolderId == null ||
    !loadedFolders.some((f) => f.id === activeFolderId)

  return (
    <div className="relative h-full w-full">
      {loadedFolders.map((folder) => (
        <div
          key={folder.id}
          className="absolute inset-0 h-full w-full"
          style={{
            display: folder.id === activeFolderId ? "block" : "none",
          }}
          aria-hidden={folder.id !== activeFolderId}
        >
          <FolderWorkspace folderId={folder.id} />
        </div>
      ))}
      {showEmpty && <EmptyFolderPane />}
    </div>
  )
}

function EmptyFolderPane() {
  return (
    <div className="flex h-full items-center justify-center text-muted-foreground text-sm">
      Select a folder to get started
    </div>
  )
}
