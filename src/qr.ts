import QRCode from "qrcode";

export interface SlintImageData {
  width: number;
  height: number;
  data: Uint8ClampedArray;
}

// Renders 1 pixel per QR module (plus quiet zone); the Slint side scales it
// up with image-rendering: pixelated for crisp edges.
export function qrToImageData(text: string): SlintImageData {
  const qr = QRCode.create(text, { errorCorrectionLevel: "M" });
  const size = qr.modules.size;
  const margin = 4;
  const dim = size + margin * 2;
  const data = new Uint8ClampedArray(dim * dim * 4).fill(255);
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      if (qr.modules.get(x, y)) {
        const i = ((y + margin) * dim + (x + margin)) * 4;
        data[i] = 0;
        data[i + 1] = 0;
        data[i + 2] = 0;
      }
    }
  }
  return { width: dim, height: dim, data };
}

export const EMPTY_IMAGE: SlintImageData = {
  width: 1,
  height: 1,
  data: new Uint8ClampedArray(4),
};
