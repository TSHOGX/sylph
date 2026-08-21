#!/usr/bin/env python3
import io, re

# Load main's sidebar (current state after git checkout --theirs)
main_path = 'frontend/components/workspace-sidebar.tsx'
s = io.open(main_path, encoding='utf-8').read()

# 1. Add DirectoryPicker import after other imports
if 'DirectoryPicker' not in s:
    anchor = 'import { useSettings } from "@/lib/settings";'
    addition = '''import { useSettings } from "@/lib/settings";
import dynamic from "next/dynamic";

const DirectoryPicker = dynamic(
  () => import("@/components/directory-picker").then((m) => m.DirectoryPicker),
  { ssr: false },
);'''
    s = s.replace(anchor, addition)
    print("✓ Added DirectoryPicker import")

# 2. Add picking state and bindWorkspace handler
if 'const [picking,' not in s:
    anchor = '  const { browseHistory, createSessionIn } = useWorkspaceUI();'
    addition = '''  const { browseHistory, createSessionIn } = useWorkspaceUI();
  const [picking, setPicking] = useState(false);
  const { updateSettings } = useSettings();

  const bindWorkspace = (path: string) => {
    updateSettings((prev) => ({
      ...prev,
      pinnedWorkspaces: [...(prev.pinnedWorkspaces ?? []), path],
    }));
    setPicking(false);
  };'''
    s = s.replace(anchor, addition)
    print("✓ Added picking state and bindWorkspace")

# 3. Add footer button before the closing </nav>
if 'New Workspace...' not in s:
    # Find the last </nav> in the file
    anchor = re.search(r'(    </nav>\n  );)', s)
    if anchor:
        footer = '''
      {/* Also offered as a button, not only on right-click: this is the sole entry
          point for binding a directory that has no sessions yet, and a hidden
          gesture is a poor way to expose the only path to a feature. */}
      <div className="shrink-0 border-t border-gray-200 p-1.5">
        <button
          type="button"
          onClick={() => setPicking(true)}
          className="flex w-full items-center gap-1.5 rounded px-1.5 py-1 text-left text-xs font-medium text-gray-600 hover:bg-gray-200/60 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
        >
          <span aria-hidden="true">+</span>
          New Workspace...
        </button>
      </div>

    </nav>
  );'''
        s = s[:anchor.start(1)] + footer
        print("✓ Added footer button")

# 4. Add DirectoryPicker dialog at the end
if 'picking && (' not in s:
    anchor = re.search(r'(export function WorkspaceSidebar)', s)
    if anchor:
        dialog = '''
      {picking && (
        <DirectoryPicker
          title="New Workspace"
          actionLabel="Bind workspace"
          onSelect={bindWorkspace}
          onCancel={() => setPicking(false)}
        />
      )}
'''
        s = s[:anchor.start()] + dialog + '\n' + s[anchor.start():]
        print("✓ Added DirectoryPicker dialog")

# 5. Add stopPropagation to archive rows' onContextMenu (if they have one)
# Main's archive rows: check if they're wrapped in a link with onContextMenu
if '.map((archived)' in s and 'stopPropagation' not in s:
    # Find archive row rendering
    pattern = r'(archived\.map\(\(row\) => \(\s+<li key={row\.id} className="group">\s+<Link[^>]+onContextMenu={\(event\) => {)(\s+})'
    if re.search(pattern, s):
        s = re.sub(pattern, r'\1\n          event.stopPropagation();\2', s)
        print("✓ Added stopPropagation to archive rows")

io.open(main_path, 'w', encoding='utf-8').write(s)
print(f"\\nwrote {main_path}")
