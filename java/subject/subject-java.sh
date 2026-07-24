#!/bin/bash
set -eu

classes="java/target/test-classes"
if [ ! -d "$classes" ]; then
  echo "subject-java: compiled classes missing; run cargo xtask test-java" >&2
  exit 2
fi

if [ -n "${JAVA_HOME:-}" ] && [ -x "$JAVA_HOME/bin/java" ]; then
  exec "$JAVA_HOME/bin/java" -cp "$classes" org.facet.vox.subject.VoxJavaSubject
fi

exec java -cp "$classes" org.facet.vox.subject.VoxJavaSubject
