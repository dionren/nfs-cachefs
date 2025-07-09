# GitHub Release Guide for NFS-CacheFS v0.2.0

## 📋 Pre-Release Checklist

✅ Code changes committed and pushed  
✅ Version updated to 0.2.0 in Cargo.toml and main.rs  
✅ CHANGELOG.md updated with v0.2.0 changes  
✅ Binary compiled and tested  
✅ Release package created and compressed  
✅ SHA256 checksum generated  
✅ Git tag v0.2.0 created  
✅ Old/unnecessary files cleaned up  

## 🚀 GitHub Release Steps

### 1. Push to GitHub
```bash
git push origin main
git push origin v0.2.0
```

### 2. Create GitHub Release

1. Go to your GitHub repository
2. Click "Releases" tab
3. Click "Create a new release"
4. Fill in the release form:

**Tag version**: `v0.2.0`  
**Release title**: `NFS-CacheFS v0.2.0 - Critical Mount Helper Fix`

**Description**: Copy content from `RELEASE_NOTES_v0.2.0.md`

### 3. Upload Release Assets

Upload these files as release assets:

1. **nfs-cachefs-v0.2.0-linux-x86_64.tar.gz** (1.0 MB)
   - Main binary package for Ubuntu 22.04/24.04 x86_64

2. **nfs-cachefs-v0.2.0-linux-x86_64.tar.gz.sha256** (105 B)
   - SHA256 checksum file

### 4. Release Settings

- ✅ Set as the latest release
- ❌ This is a pre-release (uncheck)
- ✅ Create a discussion for this release (optional)

## 📦 Release Package Contents

The `nfs-cachefs-v0.2.0-linux-x86_64.tar.gz` contains:

```
nfs-cachefs-v0.2.0-linux-x86_64/
├── nfs-cachefs          # Main binary (2.8 MB)
├── install.sh           # Installation script
├── README.md            # Documentation
├── LICENSE             # MIT license
└── CHANGELOG.md        # Version history
```

## 🔍 Verification

### Package Integrity
```bash
# Verify SHA256 checksum
sha256sum -c nfs-cachefs-v0.2.0-linux-x86_64.tar.gz.sha256
```

### Binary Information
- **Size**: 2.8 MB
- **Architecture**: x86_64
- **Version**: 0.2.0
- **Dependencies**: libfuse3-3, fuse3

### Test Installation
```bash
# Extract and test
tar -xzf nfs-cachefs-v0.2.0-linux-x86_64.tar.gz
cd nfs-cachefs-v0.2.0-linux-x86_64
./nfs-cachefs --version  # Should output: nfs-cachefs 0.2.0
```

## 📝 Release Notes Summary

**Critical Bug Fix Release**

This release fixes a critical issue with mount helper mode parameter parsing that prevented proper mounting using standard mount commands.

**Key Fixes:**
- Mount commands now correctly parse `-o` options when using `mount -t cachefs`
- Fixed `nfs_backend` parameter not being read from mount options
- Improved argument parsing logic for mount.cachefs helper mode

**Installation:**
```bash
wget https://github.com/your-org/nfs-cachefs/releases/download/v0.2.0/nfs-cachefs-v0.2.0-linux-x86_64.tar.gz
tar -xzf nfs-cachefs-v0.2.0-linux-x86_64.tar.gz
cd nfs-cachefs-v0.2.0-linux-x86_64
./install.sh
```

## 🔗 Post-Release Actions

1. Update project README.md with new download links
2. Notify users about the critical fix
3. Update documentation if needed
4. Monitor for any issues with the new release

---

**SHA256**: `bb4dd5ac683982e867f40c7d312d832729b69c272a3c696d115eed5b4a4c6aa3`