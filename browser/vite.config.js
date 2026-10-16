import { defineConfig } from 'vite';
import { viteSingleFile } from 'vite-plugin-singlefile';

export default defineConfig({
  plugins: [viteSingleFile()],
  preview: {
    allowedHosts: ['.coder.cs.odu.edu', 'coder.cs.odu.edu', 'localhost']
  },
  server: {
    allowedHosts: ['.coder.cs.odu.edu', 'coder.cs.odu.edu', 'localhost']
  },
  build: {
    chunkSizeWarningLimit: 5000
  }
});
