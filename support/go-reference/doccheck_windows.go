//go:build windows

package osutil

import (
	"errors"
	"os"
	"path/filepath"
	"sync"
	"syscall"
	"unsafe"
)

// EditoDocCheck.dll è il wrapper della libreria chk_defaced (© Dario Finardi,
// integrata su autorizzazione dell'autore): scansione deterministica
// anti-manomissione dei documenti (font defacement, testo nascosto).
// La DLL è affiancata all'eseguibile in tutti i canali; se assente la
// funzione risulta non disponibile (DocCheckAvailable() == false).

var (
	docCheckOnce sync.Once
	procDCScan   *syscall.LazyProc
	procDCFree   *syscall.LazyProc

	// kernel32 (la var `kernel32` è in singleinstance_windows.go): usati per
	// copiare la stringa C di risposta senza conversioni uintptr->Pointer.
	procLstrlenA      = kernel32.NewProc("lstrlenA")
	procRtlMoveMemory = kernel32.NewProc("RtlMoveMemory")
)

func loadDocCheckDLL() {
	docCheckOnce.Do(func() {
		exe, err := os.Executable()
		if err != nil {
			return
		}
		path := filepath.Join(filepath.Dir(exe), "EditoDocCheck.dll")
		if _, err := os.Stat(path); err != nil {
			return
		}
		dll := syscall.NewLazyDLL(path)
		if dll.Load() != nil {
			return
		}
		procDCScan = dll.NewProc("EditoDocCheckScan")
		procDCFree = dll.NewProc("EditoDocCheckFree")
	})
}

// DocCheckAvailable riporta se la DLL di scansione è presente e caricabile.
func DocCheckAvailable() bool {
	loadDocCheckDLL()
	return procDCScan != nil && procDCFree != nil
}

// DocCheckScan esegue la scansione deterministica anti-manomissione sul file e
// ritorna il Report JSON di chk_defaced (o un JSON {"error": ...} su errori di
// parsing, gestito dal chiamante). Bloccante ma rapida (~35–90 ms a documento).
func DocCheckScan(path string) (string, error) {
	loadDocCheckDLL()
	if procDCScan == nil || procDCFree == nil {
		return "", errors.New("EditoDocCheck.dll non disponibile")
	}
	p, err := syscall.UTF16PtrFromString(path)
	if err != nil {
		return "", err
	}
	r, _, _ := procDCScan.Call(uintptr(unsafe.Pointer(p)))
	if r == 0 {
		return "", errors.New("scansione fallita (risposta nulla)")
	}
	defer procDCFree.Call(r)

	// La stringa C (UTF-8, null-terminated) vive nell'allocatore della DLL:
	// copiala in memoria Go prima della Free. lstrlenA + RtlMoveMemory evitano
	// conversioni uintptr->unsafe.Pointer (vietate dal modello di memoria Go).
	length, _, _ := procLstrlenA.Call(r)
	if length == 0 {
		return "", errors.New("scansione fallita (risposta vuota)")
	}
	buf := make([]byte, length)
	procRtlMoveMemory.Call(uintptr(unsafe.Pointer(&buf[0])), r, length)
	return string(buf), nil
}
