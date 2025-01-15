#include "../hpatch-sys/HPatch/patch_types.h"
#include <stdbool.h>

typedef hpatch_TStreamOutput hdiff_TStreamOutput;
typedef hpatch_TStreamInput hdiff_TStreamInput;
// compress plugin
typedef struct hdiff_TCompress {
    // return type tag; strlen(result)<=hpatch_kMaxPluginTypeLength; (Note:result lifetime)
    const char *(*compressType)(void); // ascii cstring,cannot contain '&'
    // return the max compressed size, if input dataSize data;
    hpatch_StreamPos_t (*maxCompressedSize)(hpatch_StreamPos_t dataSize);
    // return support threadNumber
    int (*setParallelThreadNumber)(struct hdiff_TCompress *compressPlugin, int threadNum);
    // compress data to out_code; return compressed size, if error or not need compress then return 0;
    // if out_code->write() return hdiff_stream_kCancelCompress(error) then return 0;
    // if memory I/O can use hdiff_compress_mem()
    hpatch_StreamPos_t (*compress)(const struct hdiff_TCompress *compressPlugin,
                                   const hpatch_TStreamOutput *out_code,
                                   const hpatch_TStreamInput *in_data);
    const char *(*compressTypeForDisplay)(void); // like compressType but just for display,can NULL
} hdiff_TCompress;

// create a diff data between oldData and newData, the diffData saved as single compressed stream
//   kMinSingleMatchScore: default 6, bin: 0--4  text: 4--9
//   patchStepMemSize>=hpatch_kStreamCacheSize, default 256k, recommended 64k,2m etc...
//   isUseBigCacheMatch: big cache max used O(oldSize) memory, match speed faster, but build big cache slow
void create_single_compressed_diff(const unsigned char *newData, const unsigned char *newData_end,
                                   const unsigned char *oldData, const unsigned char *oldData_end,
                                   const hpatch_TStreamOutput *out_diff, const hdiff_TCompress *compressPlugin,
                                   int kMinSingleMatchScore,
                                   size_t patchStepMemSize,
                                   bool isUseBigCacheMatch,
                                   void *listener, size_t threadNum);