"use client"

import { Suspense, type ReactNode } from "react"
import { FolderNavProvider } from "@/contexts/folder-nav-context"
import { GitCredentialProvider } from "@/contexts/git-credential-context"

export default function MainLayout({ children }: { children: ReactNode }) {
  return (
    <Suspense>
      <GitCredentialProvider>
        <FolderNavProvider>{children}</FolderNavProvider>
      </GitCredentialProvider>
    </Suspense>
  )
}
