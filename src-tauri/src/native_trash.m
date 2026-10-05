// Public Foundation object identifiers bind the Carbon reference to a held
// descriptor. Never reconstruct a pathname for the actual Trash operation.
#import <Foundation/Foundation.h>
#import <CoreServices/CoreServices.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>
#include <stdlib.h>
#include <stdio.h>

typedef struct {
    int fd;
    FSRef reference;
    uint64_t device;
    uint64_t inode;
} SweepTrashPin;

static void explain(char *error, size_t capacity, const char *message) {
    if (capacity) snprintf(error, capacity, "%s", message);
}

static BOOL same_object(NSURL *url, int fd) {
    if (!url) return NO;
    NSURL *descriptor = [NSURL fileURLWithPath:
        [NSString stringWithFormat:@"/dev/fd/%d", fd]];
    id a = nil, b = nil;
    // These are opaque identifiers. Do not parse private data or URL syntax.
    return [url getResourceValue:&a forKey:NSURLFileResourceIdentifierKey error:nil]
        && [descriptor getResourceValue:&b forKey:NSURLFileResourceIdentifierKey error:nil]
        && a && b && [a isEqual:b];
}

void sweep_trash_release(SweepTrashPin *pin) {
    if (pin) { close(pin->fd); free(pin); }
}

SweepTrashPin *sweep_trash_prepare(
    const char *path, uint64_t device, uint64_t inode,
    void (*hook)(void *, int), void *context, char *error, size_t capacity
) {
    @autoreleasepool {
        int fd = open(path, O_EVTONLY | O_SYMLINK | O_CLOEXEC);
        struct stat metadata;
        if (fd < 0 || fstat(fd, &metadata) != 0
            || (uint64_t)metadata.st_dev != device || metadata.st_ino != inode) {
            if (fd >= 0) close(fd);
            explain(error, capacity, "文件身份已变化或无法安全读取，已保留项目；请重新扫描。");
            return NULL;
        }
        // Foundation's descriptor URL identity can follow a leaf link's target.
        // Refuse symlinks rather than turn a selected link into a target move.
        if (!(S_ISREG(metadata.st_mode) || S_ISDIR(metadata.st_mode))) {
            close(fd);
            explain(error, capacity, "符号链接和特殊文件仅支持在 Finder 中处理；已保留项目。");
            return NULL;
        }
        if (!S_ISDIR(metadata.st_mode) && metadata.st_nlink != 1) {
            close(fd);
            explain(error, capacity, "文件存在多个硬链接，无法确认原位置；请在 Finder 中处理。");
            return NULL;
        }
        if (hook) hook(context, 0);
        FSRef reference = {0};
        NSURL *objectURL = [NSURL fileURLWithFileSystemRepresentation:path
            isDirectory:S_ISDIR(metadata.st_mode) relativeToURL:nil].fileReferenceURL;
        if (hook) hook(context, 1);
        if (!objectURL.isFileReferenceURL || !same_object(objectURL, fd)
            || !CFURLGetFSRef((__bridge CFURLRef)objectURL, &reference)) {
            close(fd);
            explain(error, capacity, "无法绑定已验证的文件身份，已保留项目；请在 Finder 中检查。");
            return NULL;
        }
        SweepTrashPin *pin = calloc(1, sizeof *pin);
        if (!pin) {
            close(fd);
            explain(error, capacity, "无法建立移动记录，已保留项目。");
            return NULL;
        }
        pin->fd = fd; pin->reference = reference;
        pin->device = device; pin->inode = inode;
        return pin;
    }
}

int sweep_trash_move(SweepTrashPin *pin, char *destination, size_t path_capacity,
    char *error, size_t capacity) {
    @autoreleasepool {
        struct stat held;
        if (fstat(pin->fd, &held) != 0 || (uint64_t)held.st_dev != pin->device
            || held.st_ino != pin->inode
            || (!S_ISDIR(held.st_mode) && held.st_nlink != 1)) {
            explain(error, capacity, "文件链接或身份已变化，已保留项目；请重新扫描。");
            return 0;
        }
        FSRef result = {0};
        // Unsupported Trash/cross-volume moves fail; never permanently delete.
        OSStatus moved = FSMoveObjectToTrashSync(&pin->reference, &result,
            kFSFileOperationDoNotMoveAcrossVolumes);
        if (moved != noErr) {
            if (capacity) snprintf(error, capacity, "无法移入废纸篓（系统错误 %d）；请在 Finder 中检查。", (int)moved);
            return -1;
        }
        NSURL *url = (__bridge_transfer NSURL *)CFURLCreateFromFSRef(NULL, &result);
        if (!same_object(url.fileReferenceURL, pin->fd)
            || FSRefMakePath(&result, (UInt8 *)destination, (UInt32)path_capacity) != noErr) {
            explain(error, capacity, "系统移动已返回，但无法确认废纸篓回执；请在 Finder 检查，勿直接重试。");
            return -1;
        }
        struct stat metadata;
        if (lstat(destination, &metadata) != 0
            || (uint64_t)metadata.st_dev != pin->device || metadata.st_ino != pin->inode) {
            explain(error, capacity, "废纸篓回执位置已变化；请在 Finder 检查，勿直接重试。");
            return -1;
        }
        return 1;
    }
}
