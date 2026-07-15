package certs

import (
	"bytes"
	"context"
	"crypto"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"errors"
	"fmt"
	"math/big"
	"os"
	"path/filepath"
	"time"

	"github.com/gofrs/flock"
)

const (
	localCAValidity = 10 * 365 * 24 * time.Hour
	localCALockWait = 30 * time.Second
)

// LoadOrCreateManager 加载设备专属 CA；首次运行时在本地数据目录生成。
func LoadOrCreateManager(certPath, keyPath string) (*Manager, []byte, error) {
	releaseLock, err := acquireCAInitLock(keyPath + ".lock")
	if err != nil {
		return nil, nil, err
	}
	defer releaseLock()

	certPEM, certErr := os.ReadFile(certPath)
	keyPEM, keyErr := os.ReadFile(keyPath)

	switch {
	case certErr == nil && keyErr == nil:
		if err := os.Chmod(keyPath, 0o600); err != nil {
			return nil, nil, fmt.Errorf("限制 CA 私钥权限失败: %w", err)
		}
	case errors.Is(certErr, os.ErrNotExist) && errors.Is(keyErr, os.ErrNotExist):
		var err error
		certPEM, keyPEM, err = generateLocalCA()
		if err != nil {
			return nil, nil, err
		}
		if err := writeCAPair(certPath, keyPath, certPEM, keyPEM); err != nil {
			return nil, nil, err
		}
	case errors.Is(certErr, os.ErrNotExist) && keyErr == nil:
		privateKey, err := parsePrivateKeyPEM(keyPEM)
		if err != nil {
			return nil, nil, fmt.Errorf("读取现有 CA 私钥失败: %w", err)
		}
		certPEM, err = createLocalCACertificate(privateKey)
		if err != nil {
			return nil, nil, err
		}
		if err := writeFileAtomically(certPath, certPEM, 0o644); err != nil {
			return nil, nil, fmt.Errorf("恢复 CA 证书失败: %w", err)
		}
	case certErr == nil && errors.Is(keyErr, os.ErrNotExist):
		if !sameCertificatePEM(certPEM, LegacyCACertPEM()) {
			return nil, nil, fmt.Errorf("CA 私钥缺失，拒绝替换未知证书: %s", keyPath)
		}
		var err error
		certPEM, keyPEM, err = generateLocalCA()
		if err != nil {
			return nil, nil, err
		}
		if err := os.Remove(certPath); err != nil {
			return nil, nil, fmt.Errorf("移除旧版共享 CA 文件失败: %w", err)
		}
		if err := writeCAPair(certPath, keyPath, certPEM, keyPEM); err != nil {
			return nil, nil, err
		}
	case certErr != nil:
		return nil, nil, fmt.Errorf("读取 CA 证书失败: %w", certErr)
	default:
		return nil, nil, fmt.Errorf("读取 CA 私钥失败: %w", keyErr)
	}

	manager, err := NewManagerFromPEM(certPEM, keyPEM)
	if err != nil {
		return nil, nil, fmt.Errorf("加载设备 CA 失败: %w", err)
	}
	return manager, cloneBytes(certPEM), nil
}

func acquireCAInitLock(lockPath string) (func(), error) {
	if err := os.MkdirAll(filepath.Dir(lockPath), 0o700); err != nil {
		return nil, fmt.Errorf("创建 CA 数据目录失败: %w", err)
	}
	fileLock := flock.New(lockPath, flock.SetPermissions(0o600))
	ctx, cancel := context.WithTimeout(context.Background(), localCALockWait)
	defer cancel()
	locked, err := fileLock.TryLockContext(ctx, 100*time.Millisecond)
	if err != nil {
		return nil, fmt.Errorf("获取 CA 初始化锁失败: %w", err)
	}
	if !locked {
		return nil, fmt.Errorf("等待 CA 初始化锁超时: %s", lockPath)
	}
	return func() { _ = fileLock.Unlock() }, nil
}

func generateLocalCA() ([]byte, []byte, error) {
	privateKey, err := rsa.GenerateKey(rand.Reader, 3072)
	if err != nil {
		return nil, nil, fmt.Errorf("生成 CA 私钥失败: %w", err)
	}
	certPEM, err := createLocalCACertificate(privateKey)
	if err != nil {
		return nil, nil, err
	}
	keyPEM, err := marshalPrivateKeyPEM(privateKey)
	if err != nil {
		return nil, nil, fmt.Errorf("编码 CA 私钥失败: %w", err)
	}
	return certPEM, keyPEM, nil
}

func createLocalCACertificate(privateKey crypto.PrivateKey) ([]byte, error) {
	signer, ok := privateKey.(crypto.Signer)
	if !ok {
		return nil, errors.New("CA 私钥不支持签名")
	}
	serial, err := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 128))
	if err != nil {
		return nil, fmt.Errorf("生成 CA 序列号失败: %w", err)
	}
	publicKeyDER, err := x509.MarshalPKIXPublicKey(signer.Public())
	if err != nil {
		return nil, fmt.Errorf("编码 CA 公钥失败: %w", err)
	}
	subjectKeyID := sha256.Sum256(publicKeyDER)
	now := time.Now()
	template := &x509.Certificate{
		SerialNumber: serial,
		Subject: pkix.Name{
			CommonName:   "My Cursor Local CA",
			Organization: []string{"My Cursor"},
		},
		NotBefore:             now.Add(-time.Hour),
		NotAfter:              now.Add(localCAValidity),
		KeyUsage:              x509.KeyUsageDigitalSignature | x509.KeyUsageCertSign | x509.KeyUsageCRLSign,
		BasicConstraintsValid: true,
		IsCA:                  true,
		MaxPathLen:            0,
		MaxPathLenZero:        true,
		SubjectKeyId:          append([]byte(nil), subjectKeyID[:20]...),
	}
	der, err := x509.CreateCertificate(rand.Reader, template, template, signer.Public(), signer)
	if err != nil {
		return nil, fmt.Errorf("生成 CA 证书失败: %w", err)
	}
	return pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der}), nil
}

func writeCAPair(certPath, keyPath string, certPEM, keyPEM []byte) error {
	if filepath.Dir(certPath) != filepath.Dir(keyPath) {
		return errors.New("CA 证书与私钥必须位于同一目录")
	}
	if err := os.MkdirAll(filepath.Dir(keyPath), 0o700); err != nil {
		return fmt.Errorf("创建 CA 数据目录失败: %w", err)
	}
	if err := writeFileAtomically(keyPath, keyPEM, 0o600); err != nil {
		return fmt.Errorf("写入 CA 私钥失败: %w", err)
	}
	if err := writeFileAtomically(certPath, certPEM, 0o644); err != nil {
		_ = os.Remove(keyPath)
		return fmt.Errorf("写入 CA 证书失败: %w", err)
	}
	return nil
}

func writeFileAtomically(path string, content []byte, mode os.FileMode) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	temp, err := os.CreateTemp(filepath.Dir(path), ".my-cursor-ca-*")
	if err != nil {
		return err
	}
	tempPath := temp.Name()
	defer os.Remove(tempPath)
	if err := temp.Chmod(mode); err != nil {
		_ = temp.Close()
		return err
	}
	if _, err := temp.Write(content); err != nil {
		_ = temp.Close()
		return err
	}
	if err := temp.Sync(); err != nil {
		_ = temp.Close()
		return err
	}
	if err := temp.Close(); err != nil {
		return err
	}
	if err := os.Rename(tempPath, path); err != nil {
		return err
	}
	return os.Chmod(path, mode)
}

func parsePrivateKeyPEM(keyPEM []byte) (crypto.PrivateKey, error) {
	block, _ := pem.Decode(keyPEM)
	if block == nil {
		return nil, errors.New("invalid CA key PEM")
	}
	switch block.Type {
	case "RSA PRIVATE KEY":
		return x509.ParsePKCS1PrivateKey(block.Bytes)
	case "EC PRIVATE KEY":
		return x509.ParseECPrivateKey(block.Bytes)
	case "PRIVATE KEY":
		return x509.ParsePKCS8PrivateKey(block.Bytes)
	default:
		return nil, errors.New("unsupported CA key format")
	}
}

func validateCAPair(cert *x509.Certificate, privateKey crypto.PrivateKey) error {
	if cert == nil || !cert.IsCA || cert.KeyUsage&x509.KeyUsageCertSign == 0 {
		return errors.New("certificate is not a signing CA")
	}
	signer, ok := privateKey.(crypto.Signer)
	if !ok {
		return errors.New("CA private key does not support signing")
	}
	certPublicKey, err := x509.MarshalPKIXPublicKey(cert.PublicKey)
	if err != nil {
		return err
	}
	keyPublicKey, err := x509.MarshalPKIXPublicKey(signer.Public())
	if err != nil {
		return err
	}
	if !bytes.Equal(certPublicKey, keyPublicKey) {
		return errors.New("CA certificate and private key do not match")
	}
	return nil
}

func sameCertificatePEM(left, right []byte) bool {
	leftBlock, _ := pem.Decode(left)
	rightBlock, _ := pem.Decode(right)
	if leftBlock == nil || rightBlock == nil {
		return false
	}
	leftCert, leftErr := x509.ParseCertificate(leftBlock.Bytes)
	rightCert, rightErr := x509.ParseCertificate(rightBlock.Bytes)
	return leftErr == nil && rightErr == nil && bytes.Equal(leftCert.Raw, rightCert.Raw)
}
