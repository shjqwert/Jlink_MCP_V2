#include <stdint.h>

/** Nested sample state used to validate member and multidimensional array metadata. */
typedef struct
{
    uint32_t ulSequence;        /**< Monotonic fixture sequence value. */
    int16_t  awMatrix[2][3];    /**< Signed two-dimensional sample matrix. */
} ST_F0C_NESTED;

/** Alternate payload views used to validate union metadata. */
typedef union
{
    uint32_t ulRawValue;        /**< Unsigned raw payload view. */
    float    fPhysicalValue;    /**< Floating-point payload view. */
    uint8_t  aucBytes[4];       /**< Byte-wise payload view. */
} UN_F0C_PAYLOAD;

/** Alternate double-precision views used to validate non-finite values. */
typedef union
{
    uint64_t ullRawValue;     /**< Unsigned raw double-precision view. */
    double   dPhysicalValue;  /**< Double-precision floating-point view. */
    uint8_t  aucBytes[8];     /**< Byte-wise double-precision view. */
} UN_F0C_DOUBLE_PAYLOAD;

/** Packed logical flags used to validate bit-field offsets and widths. */
typedef struct
{
    unsigned int uiReadyFlg : 1;  /**< Ready state flag. */
    signed int   iDelta     : 5;  /**< Signed five-bit delta value. */
    unsigned int uiMode     : 3;  /**< Operating mode value. */
    unsigned int uiReserved : 23; /**< Reserved bits completing the storage unit. */
} ST_F0C_FLAGS;

/** Root fixture object used to validate nested aggregate traversal. */
typedef struct
{
    ST_F0C_NESTED  stNested;   /**< Nested scalar and matrix state. */
    UN_F0C_PAYLOAD unPayload;  /**< Alternate payload representation. */
    ST_F0C_FLAGS   stFlags;    /**< Packed logical flags. */
    uint64_t       ullCounter; /**< Unsigned 64-bit counter value. */
    int64_t        llOffset;   /**< Signed 64-bit offset value. */
    float          fFloat;     /**< Single-precision sample value. */
    double         dDouble;    /**< Double-precision sample value. */
} ST_F0C_ROOT;

/** Variable-length packet used to validate bounded flexible-array slices. */
typedef struct
{
    uint16_t uwLength;     /**< Payload length in bytes. */
    uint8_t  aucPayload[]; /**< Variable-length payload bytes. */
} ST_F0C_FLEX;

/** Root object retained in the IAR DWARF fixture. */
volatile ST_F0C_ROOT gstF0cRoot =
{
    {7u, {{-3, -2, -1}, {1, 2, 3}}},
    {0x3F800000u},
    {1u, -7, 5u, 0u},
    0xFEDCBA9876543210u,
    -0x0123456789ABCDELL,
    1.25F,
    -2.5
};

/** Flexible-array header placed at a stable fixture address. */
__root volatile ST_F0C_FLEX gstF0cFlex @ 0x20001000 = {6u};

/** Flexible-array payload placed immediately after the header. */
__root volatile uint8_t gaucF0cFlexPayload[6] @ 0x20001002 = {11u, 22u, 33u, 44u, 55u, 66u};

/** Single-precision NaN and positive infinity bit patterns. */
volatile UN_F0C_PAYLOAD gaunF0cFloatSpecial[2] = {{0x7FC00000u}, {0x7F800000u}};

/** Double-precision NaN and positive infinity bit patterns. */
volatile UN_F0C_DOUBLE_PAYLOAD gaunF0cDoubleSpecial[2] = {{0x7FF8000000000000u}, {0x7FF0000000000000u}};

/** @brief Keep every fixture declaration reachable in emitted debug information. */
void F0cDwarfFixtureHold(void)
{
    gstF0cRoot.stNested.ulSequence++;
    gstF0cFlex.uwLength = gaucF0cFlexPayload[0];
}
