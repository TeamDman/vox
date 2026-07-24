@echo off
setlocal
set "SFM_CLASSES=java\target\test-classes"
if not exist "%SFM_CLASSES%\" (
  echo subject-java: compiled classes missing; run cargo xtask test-java 1>&2
  exit /b 2
)
if defined JAVA_HOME if exist "%JAVA_HOME%\bin\java.exe" goto java_home
java.exe -cp "%SFM_CLASSES%" org.facet.vox.subject.VoxJavaSubject
exit /b %ERRORLEVEL%

:java_home
"%JAVA_HOME%\bin\java.exe" -cp "%SFM_CLASSES%" org.facet.vox.subject.VoxJavaSubject
exit /b %ERRORLEVEL%
