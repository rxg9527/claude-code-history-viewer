import { GlobalSearchModal } from "./GlobalSearchModal";
import { useModal } from "@/contexts/modal";

export const GlobalSearchModalContainer: React.FC = () => {
    const { isOpen, closeModal } = useModal();
    const globalSearchOpen = isOpen("globalSearch");

    return (
        <GlobalSearchModal
            isOpen={globalSearchOpen}
            onClose={() => closeModal("globalSearch")}
        />
    );
};
