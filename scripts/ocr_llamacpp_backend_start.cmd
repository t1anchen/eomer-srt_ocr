@echo off
rem Template: start the local llama.cpp backend used by eomer-srt-ocr.
rem
rem Copy this file somewhere outside the repository (for example next to the
rem binary) and fill in the three paths for the current machine. The filled-in
rem copy is machine specific and must not be committed.
rem
rem Example of a filled-in copy:
rem   set LLAMA_BIN=C:\path\to\llama-bXXXXX-bin-win-cuda-12.4-x64
rem   set MODEL=C:\path\to\models\model.gguf
rem   set MMPROJ=C:\path\to\models\mmproj-model.gguf

set LLAMA_BIN=CHANGE_ME-llama-bin-directory
set MODEL=CHANGE_ME-model.gguf
set MMPROJ=CHANGE_ME-mmproj.gguf
set PORT=8080

if not exist "%LLAMA_BIN%\llama-server.exe" (
  echo [error] llama-server.exe not found under "%LLAMA_BIN%"
  echo         edit LLAMA_BIN at the top of this file
  exit /b 1
)
if not exist "%MODEL%" (
  echo [error] model not found: "%MODEL%"
  echo         edit MODEL at the top of this file
  exit /b 1
)
if not exist "%MMPROJ%" (
  echo [error] mmproj not found: "%MMPROJ%"
  echo         edit MMPROJ at the top of this file
  exit /b 1
)

rem Refuse to start when the port is already taken: a second server would
rem compete for the same port and pin a second copy of the model in VRAM.
netstat -ano | findstr /R /C:"127.0.0.1:%PORT% *LISTENING" >nul 2>&1
if not errorlevel 1 (
  echo [error] port %PORT% is already in use - a backend is probably running.
  echo         check with:  netstat -ano ^| findstr :%PORT%
  echo         stop the existing one before starting another.
  exit /b 1
)

"%LLAMA_BIN%\llama-server.exe" ^
  -m "%MODEL%" ^
  --mmproj "%MMPROJ%" ^
  --host 127.0.0.1 --port %PORT% ^
  -ngl 99 ^
  -c 8192
