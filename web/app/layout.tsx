import type { Metadata, Viewport } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "OMAR Mission Control",
  description: "Build and observe principled OMAR workflows.",
  // Same mark as the navbar.
  icons: { icon: "/omar-logo.png", apple: "/omar-logo.png" },
};

export const viewport: Viewport = {
  themeColor: "#08080a",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    // The theme attribute is added on the client before paint, so the server
    // markup is allowed to differ from it.
    <html lang="en" suppressHydrationWarning>
      <head>
        <script
          // Light is opt-in and kept in local storage; set it before the first
          // paint so a light page never flashes dark. The key is THEME_KEY in
          // settings-menu.tsx, spelled out here because this runs before any
          // module does.
          dangerouslySetInnerHTML={{
            __html:
              'try{if(localStorage.getItem("omar.theme")==="light")document.documentElement.dataset.theme="light"}catch(e){}',
          }}
        />
      </head>
      <body>{children}</body>
    </html>
  );
}
