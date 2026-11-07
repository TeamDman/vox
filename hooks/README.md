# Git hooks are disabled in this fork

The inherited hook scripts are retained with a `.disabled` suffix for reference.
Git does not recognize those names as hooks. Do not rename them back or install
new Git hooks; run checks explicitly instead.

Fresh clones do not install Git hooks. Existing clones may still have copies in
`.git/hooks`, because Git does not distribute or remove that directory. After
pulling this change, run `bash hooks/install.sh` once in each existing clone to
point Git at an unused hooks directory. This disables all hooks for that clone
without deleting the old copies or changing global Git configuration. Repeating
this command is safe. The old `scripts/setup.sh` entry point, when present, does
the same thing and no longer installs hooks.

This configuration stays effective when switching branches. Older branches can
still contain the old installers, so update to a branch containing this fix
before running setup.
