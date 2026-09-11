/* Delay-load failure hook. windows-rs 0.61+ raw-dylibs CoTaskMemAlloc from
 * combase.dll (microsoft/windows-rs#3808); Windows 7 exports it from ole32.dll. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <delayimp.h>
#include <string.h>

static FARPROC WINAPI kachina_delay_hook(unsigned notify, DelayLoadInfo *info) {
    if (notify != dliFailLoadLib || info == NULL || info->szDll == NULL) {
        return 0;
    }
    if (_stricmp(info->szDll, "combase.dll") == 0) {
        return (FARPROC)LoadLibraryW(L"ole32.dll");
    }
    return 0;
}

const PfnDliHook __pfnDliFailureHook2 = kachina_delay_hook;
