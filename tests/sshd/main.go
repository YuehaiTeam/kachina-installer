// Minimal SSH test server for kachina-installer e2e:
//   - password auth (fixed user/pass)
//   - host key type selectable via -hostkey; RSA is the default because that is
//     what the production nodes run, and the client's minimal algorithm set
//     accepts only rsa-sha2-256 and ecdsa-sha2-nistp256
//   - direct-tcpip: always dials the built-in local HTTP file server ("tunnel" alias)
//   - sftp subsystem: hand-rolled SFTP v3 read-only subset (INIT/OPEN/STAT/READ/CLOSE)
//     with optional short-read injection (-shortread) to exercise client gap re-requests
//
// Prints FINGERPRINT=<hex sha256 of host key blob> and READY=1 on stdout.
package main

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"flag"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"

	"golang.org/x/crypto/ssh"
)

var (
	addr      = flag.String("addr", "127.0.0.1:18122", "ssh listen address")
	httpAddr  = flag.String("http", "127.0.0.1:18180", "internal http file server address")
	root      = flag.String("root", ".", "file root served via http and sftp")
	user      = flag.String("user", "test", "ssh username")
	pass      = flag.String("pass", "pass123", "ssh password")
	hostKey   = flag.String("hostkey", "rsa", "host key type: rsa (matches production) or ecdsa")
	shortRead = flag.Bool("shortread", false, "answer READ with at most half the requested bytes")
)

func main() {
	flag.Parse()

	signer, err := newHostKeySigner(*hostKey)
	if err != nil {
		log.Fatal(err)
	}
	fp := sha256.Sum256(signer.PublicKey().Marshal())
	fmt.Printf("FINGERPRINT=%s\n", hex.EncodeToString(fp[:]))

	go func() {
		log.Fatal(http.ListenAndServe(*httpAddr, http.FileServer(http.Dir(*root))))
	}()

	config := &ssh.ServerConfig{
		PasswordCallback: func(c ssh.ConnMetadata, p []byte) (*ssh.Permissions, error) {
			if c.User() == *user && string(p) == *pass {
				return nil, nil
			}
			return nil, fmt.Errorf("auth denied")
		},
	}
	config.AddHostKey(signer)

	ln, err := net.Listen("tcp", *addr)
	if err != nil {
		log.Fatal(err)
	}
	fmt.Printf("READY=1 SSH=%s HTTP=%s\n", *addr, *httpAddr)
	for {
		conn, err := ln.Accept()
		if err != nil {
			log.Fatal(err)
		}
		go handleConn(conn, config)
	}
}

func newHostKeySigner(kind string) (ssh.Signer, error) {
	switch kind {
	case "rsa":
		key, err := rsa.GenerateKey(rand.Reader, 2048)
		if err != nil {
			return nil, err
		}
		return ssh.NewSignerFromKey(key)
	case "ecdsa":
		key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
		if err != nil {
			return nil, err
		}
		return ssh.NewSignerFromKey(key)
	default:
		return nil, fmt.Errorf("unknown -hostkey %q (want rsa or ecdsa)", kind)
	}
}

func handleConn(c net.Conn, config *ssh.ServerConfig) {
	sconn, chans, reqs, err := ssh.NewServerConn(c, config)
	if err != nil {
		return
	}
	defer sconn.Close()
	go ssh.DiscardRequests(reqs)
	for newCh := range chans {
		switch newCh.ChannelType() {
		case "direct-tcpip":
			go handleDirectTcpip(newCh)
		case "session":
			go handleSession(newCh)
		default:
			newCh.Reject(ssh.UnknownChannelType, "unsupported channel type")
		}
	}
}

func handleDirectTcpip(newCh ssh.NewChannel) {
	ch, reqs, err := newCh.Accept()
	if err != nil {
		return
	}
	go ssh.DiscardRequests(reqs)
	// "tunnel" alias: whatever host was requested, dial the local http server
	remote, err := net.Dial("tcp", *httpAddr)
	if err != nil {
		ch.Close()
		return
	}
	go func() {
		io.Copy(remote, ch)
		remote.(*net.TCPConn).CloseWrite()
	}()
	io.Copy(ch, remote)
	ch.Close()
	remote.Close()
}

func handleSession(newCh ssh.NewChannel) {
	ch, reqs, err := newCh.Accept()
	if err != nil {
		return
	}
	go func() {
		for req := range reqs {
			if req.Type == "subsystem" && len(req.Payload) > 4 && string(req.Payload[4:]) == "sftp" {
				req.Reply(true, nil)
				sftpServe(ch)
				return
			}
			req.Reply(false, nil)
		}
	}()
}

// ---- minimal SFTP v3 server (read-only) ----

const (
	fxpInit    = 1
	fxpVersion = 2
	fxpOpen    = 3
	fxpClose   = 4
	fxpRead    = 5
	fxpStat    = 17
	fxpStatus  = 101
	fxpHandle  = 102
	fxpData    = 103
	fxpAttrs   = 105

	fxOK          = 0
	fxEOF         = 1
	fxNoSuchFile  = 2
	fxPermDenied  = 3
	fxFailure     = 4
	attrSizeFlag  = 0x00000001
	maxPacketSize = 1 << 20
)

func sftpServe(ch ssh.Channel) {
	defer ch.Close()
	files := map[string]*os.File{}
	nextHandle := 0
	defer func() {
		for _, f := range files {
			f.Close()
		}
	}()

	for {
		pkt, err := readPacket(ch)
		if err != nil {
			return
		}
		if len(pkt) < 1 {
			return
		}
		ptype, body := pkt[0], pkt[1:]
		switch ptype {
		case fxpInit:
			writePacket(ch, []byte{fxpVersion, 0, 0, 0, 3})
		case fxpStat:
			id, rest := getU32(body)
			path, _ := getString(rest)
			info, err := os.Stat(resolve(string(path)))
			if err != nil {
				writeStatus(ch, id, fxNoSuchFile, err.Error())
				continue
			}
			out := []byte{fxpAttrs}
			out = binary.BigEndian.AppendUint32(out, id)
			out = binary.BigEndian.AppendUint32(out, attrSizeFlag)
			out = binary.BigEndian.AppendUint64(out, uint64(info.Size()))
			writePacket(ch, out)
		case fxpOpen:
			id, rest := getU32(body)
			path, _ := getString(rest)
			f, err := os.Open(resolve(string(path)))
			if err != nil {
				writeStatus(ch, id, fxNoSuchFile, err.Error())
				continue
			}
			handle := fmt.Sprintf("h%d", nextHandle)
			nextHandle++
			files[handle] = f
			out := []byte{fxpHandle}
			out = binary.BigEndian.AppendUint32(out, id)
			out = appendString(out, []byte(handle))
			writePacket(ch, out)
		case fxpRead:
			id, rest := getU32(body)
			handle, rest := getString(rest)
			offset := binary.BigEndian.Uint64(rest)
			length := binary.BigEndian.Uint32(rest[8:])
			f, ok := files[string(handle)]
			if !ok {
				writeStatus(ch, id, fxFailure, "bad handle")
				continue
			}
			if *shortRead && length > 16 {
				length /= 2
			}
			buf := make([]byte, length)
			n, err := f.ReadAt(buf, int64(offset))
			if n == 0 {
				if err == io.EOF {
					writeStatus(ch, id, fxEOF, "eof")
				} else {
					writeStatus(ch, id, fxFailure, fmt.Sprint(err))
				}
				continue
			}
			out := []byte{fxpData}
			out = binary.BigEndian.AppendUint32(out, id)
			out = appendString(out, buf[:n])
			writePacket(ch, out)
		case fxpClose:
			id, rest := getU32(body)
			handle, _ := getString(rest)
			if f, ok := files[string(handle)]; ok {
				f.Close()
				delete(files, string(handle))
			}
			writeStatus(ch, id, fxOK, "ok")
		default:
			// id-carrying unknown request → FX_OP_UNSUPPORTED-ish failure
			if len(body) >= 4 {
				id, _ := getU32(body)
				writeStatus(ch, id, fxPermDenied, "unsupported")
			}
		}
	}
}

// resolve maps the SFTP path ("/file.bin") into the -root directory.
func resolve(p string) string {
	clean := filepath.Clean("/" + strings.TrimPrefix(p, "/"))
	return filepath.Join(*root, clean)
}

func readPacket(r io.Reader) ([]byte, error) {
	var lenBuf [4]byte
	if _, err := io.ReadFull(r, lenBuf[:]); err != nil {
		return nil, err
	}
	n := binary.BigEndian.Uint32(lenBuf[:])
	if n == 0 || n > maxPacketSize {
		return nil, fmt.Errorf("bad packet length %d", n)
	}
	buf := make([]byte, n)
	if _, err := io.ReadFull(r, buf); err != nil {
		return nil, err
	}
	return buf, nil
}

func writePacket(w io.Writer, body []byte) error {
	out := binary.BigEndian.AppendUint32(nil, uint32(len(body)))
	out = append(out, body...)
	_, err := w.Write(out)
	return err
}

func writeStatus(w io.Writer, id, code uint32, msg string) {
	out := []byte{fxpStatus}
	out = binary.BigEndian.AppendUint32(out, id)
	out = binary.BigEndian.AppendUint32(out, code)
	out = appendString(out, []byte(msg))
	out = appendString(out, []byte("en"))
	writePacket(w, out)
}

func getU32(b []byte) (uint32, []byte) {
	return binary.BigEndian.Uint32(b), b[4:]
}

func getString(b []byte) ([]byte, []byte) {
	n := binary.BigEndian.Uint32(b)
	return b[4 : 4+n], b[4+n:]
}

func appendString(out, s []byte) []byte {
	out = binary.BigEndian.AppendUint32(out, uint32(len(s)))
	return append(out, s...)
}
